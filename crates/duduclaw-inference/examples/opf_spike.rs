//! Feasibility spike: run OpenAI Privacy Filter (`openai/privacy-filter`) natively
//! in Rust via ONNX Runtime, and measure CPU latency + zh-TW detection quality.
//!
//! NOT production code. Run with:
//!   cargo run -p duduclaw-inference --features onnx --example opf_spike --release
//!
//! Model dir: ~/.duduclaw/models/privacy-filter/
//!   onnx/model_q4.onnx  +  onnx/model_q4.onnx_data  (external data)
//!   tokenizer.json  config.json  viterbi_calibration.json

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

/// Byte-range slice that snaps outward to char boundaries (never panics on CJK).
fn safe_slice(s: &str, a: usize, b: usize) -> &str {
    let mut a = a.min(s.len());
    let mut b = b.min(s.len());
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    while a > 0 && !s.is_char_boundary(a) {
        a -= 1;
    }
    while b < s.len() && !s.is_char_boundary(b) {
        b += 1;
    }
    &s[a..b]
}

fn rss_kb() -> Option<u64> {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

// ---------------------------------------------------------------------------
// Label scheme (BIOES over 8 entity types + O)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    O,
    B,
    I,
    E,
    S,
}

#[derive(Clone)]
struct LabelInfo {
    name: String,
    tag: Tag,
    entity: String, // "" for O
}

fn parse_labels(id2label: &HashMap<usize, String>) -> Vec<LabelInfo> {
    let n = id2label.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let name = id2label
            .get(&i)
            .cloned()
            .unwrap_or_else(|| panic!("id2label missing id {i}"));
        let (tag, entity) = if name == "O" {
            (Tag::O, String::new())
        } else {
            let (p, rest) = name.split_once('-').expect("label must be TAG-entity");
            let t = match p {
                "B" => Tag::B,
                "I" => Tag::I,
                "E" => Tag::E,
                "S" => Tag::S,
                other => panic!("unknown tag prefix {other}"),
            };
            (t, rest.to_string())
        };
        out.push(LabelInfo { name, tag, entity });
    }
    out
}

/// BIOES constraint matrix. `allowed[from][to]`.
/// O      -> O | B-x | S-x
/// B-x    -> I-x | E-x
/// I-x    -> I-x | E-x
/// E-x/S-x-> O | B-y | S-y
fn build_transitions(labels: &[LabelInfo]) -> Vec<Vec<bool>> {
    let n = labels.len();
    let mut m = vec![vec![false; n]; n];
    for (i, from) in labels.iter().enumerate() {
        for (j, to) in labels.iter().enumerate() {
            let ok = match from.tag {
                Tag::O | Tag::E | Tag::S => matches!(to.tag, Tag::O | Tag::B | Tag::S),
                Tag::B | Tag::I => {
                    matches!(to.tag, Tag::I | Tag::E) && to.entity == from.entity
                }
            };
            m[i][j] = ok;
        }
    }
    m
}

fn start_allowed(l: &LabelInfo) -> bool {
    matches!(l.tag, Tag::O | Tag::B | Tag::S)
}
fn end_allowed(l: &LabelInfo) -> bool {
    matches!(l.tag, Tag::O | Tag::E | Tag::S)
}

fn log_softmax_row(row: &[f32]) -> Vec<f32> {
    let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
    let lse = max + sum.ln();
    row.iter().map(|v| v - lse).collect()
}

/// Constrained Viterbi. `emis[t][k]` = log-softmax emission. All transition biases 0.
fn viterbi(emis: &[Vec<f32>], labels: &[LabelInfo], trans: &[Vec<bool>]) -> Vec<usize> {
    let t_len = emis.len();
    let n = labels.len();
    if t_len == 0 {
        return vec![];
    }
    const NEG: f32 = -1.0e30;
    let mut dp = vec![vec![NEG; n]; t_len];
    let mut bp = vec![vec![0usize; n]; t_len];

    for k in 0..n {
        if start_allowed(&labels[k]) {
            dp[0][k] = emis[0][k];
        }
    }
    for t in 1..t_len {
        for k in 0..n {
            let mut best = NEG;
            let mut arg = 0usize;
            for j in 0..n {
                if dp[t - 1][j] <= NEG || !trans[j][k] {
                    continue;
                }
                let s = dp[t - 1][j];
                if s > best {
                    best = s;
                    arg = j;
                }
            }
            if best > NEG {
                dp[t][k] = best + emis[t][k];
                bp[t][k] = arg;
            }
        }
    }
    let mut best = NEG;
    let mut last = 0usize;
    for k in 0..n {
        if end_allowed(&labels[k]) && dp[t_len - 1][k] > best {
            best = dp[t_len - 1][k];
            last = k;
        }
    }
    if best <= NEG {
        // Degenerate: fall back to unconstrained end.
        for k in 0..n {
            if dp[t_len - 1][k] > best {
                best = dp[t_len - 1][k];
                last = k;
            }
        }
    }
    let mut path = vec![0usize; t_len];
    path[t_len - 1] = last;
    for t in (1..t_len).rev() {
        path[t - 1] = bp[t][path[t]];
    }
    path
}

#[derive(Clone, Debug)]
struct Span {
    label: String,
    start: usize, // byte offset in original text
    end: usize,
    text: String,
}

/// Turn a per-token BIOES path into char/byte spans using tokenizer offsets.
/// Tolerant: works for both the Viterbi (well-formed) and argmax (possibly malformed) paths.
fn path_to_spans(
    path: &[usize],
    labels: &[LabelInfo],
    offsets: &[(usize, usize)],
    text: &str,
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut open: Option<(String, usize, usize)> = None; // entity, start, end

    let flush = |open: &mut Option<(String, usize, usize)>, spans: &mut Vec<Span>| {
        if let Some((ent, s, e)) = open.take() {
            spans.push(Span {
                label: ent,
                start: s,
                end: e,
                text: safe_slice(text, s, e).to_string(),
            });
        }
    };

    for (t, &k) in path.iter().enumerate() {
        let li = &labels[k];
        let (os, oe) = offsets[t];
        match li.tag {
            Tag::O => flush(&mut open, &mut spans),
            Tag::S => {
                flush(&mut open, &mut spans);
                spans.push(Span {
                    label: li.entity.clone(),
                    start: os,
                    end: oe,
                    text: safe_slice(text, os, oe).to_string(),
                });
            }
            Tag::B => {
                flush(&mut open, &mut spans);
                open = Some((li.entity.clone(), os, oe));
            }
            Tag::I => match open.as_mut() {
                Some((ent, _, e)) if *ent == li.entity => *e = oe,
                _ => {
                    flush(&mut open, &mut spans);
                    open = Some((li.entity.clone(), os, oe));
                }
            },
            Tag::E => {
                match open.as_mut() {
                    Some((ent, _, e)) if *ent == li.entity => *e = oe,
                    _ => {
                        flush(&mut open, &mut spans);
                        open = Some((li.entity.clone(), os, oe));
                    }
                }
                flush(&mut open, &mut spans);
            }
        }
    }
    flush(&mut open, &mut spans);
    spans.retain(|s| s.end > s.start);
    spans
}

// ---------------------------------------------------------------------------
// Synthetic sample set with embedded ground truth
// ---------------------------------------------------------------------------

struct Sample {
    text: String,
    /// (entity label, byte_start, byte_end)
    gt: Vec<(String, usize, usize)>,
}

const CATS: [&str; 8] = [
    "private_person",
    "private_address",
    "private_phone",
    "private_date",
    "private_email",
    "account_number",
    "private_url",
    "secret",
];

fn pools_zh() -> HashMap<&'static str, Vec<&'static str>> {
    HashMap::from([
        (
            "private_person",
            vec![
                "王小明", "陳雅婷", "林志豪", "黃淑芬", "張家瑋", "李佩珊", "吳建宏", "劉美玲",
                "蔡宗翰", "鄭雅文",
            ],
        ),
        (
            "private_address",
            vec![
                "台北市信義區松仁路100號8樓",
                "新北市板橋區文化路一段188號3樓",
                "台中市西屯區台灣大道三段301號12樓之2",
                "高雄市前鎮區成功二路25號",
                "桃園市中壢區中大路300號",
                "台南市東區長榮路三段175巷12號5樓",
                "新竹市東區光復路二段101號",
                "宜蘭縣羅東鎮中正北路18號",
            ],
        ),
        (
            "private_phone",
            vec![
                "0912-345-678",
                "(02)2345-6789",
                "0987654321",
                "03-4267123",
                "+886-928-114-522",
                "0937-221-908",
                "(07)336-1122",
            ],
        ),
        (
            "private_date",
            vec![
                "1990年3月5日",
                "1985/07/21",
                "2001-12-03",
                "民國七十九年四月十二日",
                "1976年11月30日",
                "1994/02/28",
                "2003年9月17日",
            ],
        ),
        (
            "private_email",
            vec![
                "xiaoming.wang@example.com.tw",
                "ya.ting.chen@gmail.com",
                "jhlin@mail.ntu.edu.tw",
                "sf.huang@corp-taiwan.co",
                "chiawei.chang@outlook.com",
                "peishan.lee@hotmail.com.tw",
            ],
        ),
        (
            "account_number",
            vec![
                "012-34567890123",
                "700-0012345678901",
                "822-1234567890",
                "4532-8821-0099-1234",
                "005-0987654321098",
                "808-2233445566",
            ],
        ),
        (
            "private_url",
            vec![
                "https://portal.example.com.tw/user/93321",
                "http://intranet.corp.local/hr/profile?id=8812",
                "https://drive.google.com/file/d/1AbCdEfGh/view",
                "https://crm.dudustudio.monster/leads/4471",
                "https://www.example.org/members/chen-ya-ting",
            ],
        ),
        (
            "secret",
            vec![
                "sk-proj-9aFbQ2mZ1xLpR7TvKe3NuY8W",
                "ghp_8sKdlWqZ12Ab3Cd4Ef5Gh6Ij7Kl8Mn9Op",
                "AKIAIOSFODNN7EXAMPLE",
                "P@ssw0rd!2024Taipei",
                "xoxb-2291-88123-QwErTyUiOpAsDfGh",
            ],
        ),
    ])
}

fn pools_en() -> HashMap<&'static str, Vec<&'static str>> {
    HashMap::from([
        (
            "private_person",
            vec![
                "Daniel Hoffman",
                "Priya Raghunathan",
                "Marcus Oyelaran",
                "Sofia Bergqvist",
                "Kenji Nakamura",
                "Elena Petrova",
                "Thomas Whitfield",
                "Aisha Mahmoud",
            ],
        ),
        (
            "private_address",
            vec![
                "2140 Harrison Street, Apt 5B, San Francisco, CA 94110",
                "17 Birchwood Lane, Leeds LS8 2QR, United Kingdom",
                "Kaiserstrasse 88, 60329 Frankfurt am Main, Germany",
                "Unit 12, 340 George Street, Sydney NSW 2000",
                "9 Rue de la Paix, 75002 Paris, France",
            ],
        ),
        (
            "private_phone",
            vec![
                "+1 (415) 555-0182",
                "020 7946 0991",
                "+49 69 1234 5678",
                "(212) 555-7731",
                "+61 2 9374 4000",
                "415-555-2201",
            ],
        ),
        (
            "private_date",
            vec![
                "March 5, 1990",
                "21/07/1985",
                "2001-12-03",
                "November 30, 1976",
                "02/28/1994",
                "17 September 2003",
            ],
        ),
        (
            "private_email",
            vec![
                "daniel.hoffman@example.com",
                "p.raghunathan@acme-corp.io",
                "m.oyelaran@gmail.com",
                "sofia.bergqvist@nordic-bank.se",
                "kenji.nakamura@example.co.jp",
            ],
        ),
        (
            "account_number",
            vec![
                "GB29 NWBK 6016 1331 9268 19",
                "4532-8821-0099-1234",
                "DE89 3704 0044 0532 0130 00",
                "021000021-9876543210",
                "1234567890123456",
            ],
        ),
        (
            "private_url",
            vec![
                "https://portal.example.com/user/93321",
                "http://intranet.acme.local/hr/profile?id=8812",
                "https://drive.google.com/file/d/1AbCdEfGh/view",
                "https://crm.example.io/leads/4471",
            ],
        ),
        (
            "secret",
            vec![
                "sk-proj-9aFbQ2mZ1xLpR7TvKe3NuY8W",
                "ghp_8sKdlWqZ12Ab3Cd4Ef5Gh6Ij7Kl8Mn9Op",
                "AKIAIOSFODNN7EXAMPLE",
                "Tr0ub4dor&3-Frankfurt",
                "xoxb-2291-88123-QwErTyUiOpAsDfGh",
            ],
        ),
    ])
}

const TPL_ZH: [&str; 12] = [
    "客戶{private_person}於{private_date}來電詢問訂單進度，聯絡電話為{private_phone}，電子郵件{private_email}，寄送地址是{private_address}，請業務同仁於今日下班前回覆處理結果。",
    "退款申請：申請人{private_person}，出生日期{private_date}，匯款帳號{account_number}，行動電話{private_phone}。請財務部門核對後於三個工作天內完成撥款。",
    "會員資料更新通知——{private_person}已將聯絡信箱改為{private_email}，居住地址更新為{private_address}，如需查詢會員頁面請至{private_url}。",
    "系統維運紀錄：工程師{private_person}於{private_date}使用金鑰 {secret} 登入正式環境，相關工單連結 {private_url}，如有異常請撥{private_phone}。",
    "保單受益人變更申請書：要保人{private_person}（生日{private_date}），受款帳戶{account_number}，通訊地址{private_address}，聯絡方式{private_phone}。",
    "人資部門通知：新進同仁{private_person}報到日為{private_date}，公司信箱已開通為{private_email}，薪轉帳戶{account_number}，請人事於系統{private_url}完成建檔。",
    "客訴案件編號 A-2291：客戶{private_person}反映商品未送達，收件地址為{private_address}，留存電話{private_phone}，案件詳情請見{private_url}。",
    "醫療預約提醒：{private_person}您好，您預約的門診時間為{private_date}，若需改期請致電{private_phone}或來信{private_email}，診所位置在{private_address}。",
    "銀行對帳單寄送設定：戶名{private_person}，帳號{account_number}，寄送地址{private_address}，電子帳單信箱{private_email}。",
    "開發環境交接備忘：前任負責人{private_person}，交接日{private_date}，API 金鑰為 {secret}，後台網址 {private_url}，緊急聯絡電話{private_phone}。",
    "房屋租賃契約摘要：承租人{private_person}，身分驗證日期{private_date}，租賃標的位於{private_address}，押金退還帳戶{account_number}，聯絡電話{private_phone}。",
    "訂位確認信：{private_person}先生／小姐您好，您於{private_date}的訂位已確認，餐廳地址{private_address}，如需取消請來信{private_email}或撥打{private_phone}，線上管理連結{private_url}。",
];

const TPL_EN: [&str; 12] = [
    "Customer {private_person} called on {private_date} about order #88213. Reachable at {private_phone} or {private_email}. Shipping address on file: {private_address}. Please follow up before end of day.",
    "Refund request received. Applicant: {private_person}, date of birth {private_date}, remittance account {account_number}, contact number {private_phone}. Finance to settle within three business days.",
    "Profile update notice: {private_person} changed the contact email to {private_email} and the residential address to {private_address}. The member page is available at {private_url}.",
    "Ops log entry: engineer {private_person} signed into production on {private_date} using key {secret}. Incident ticket: {private_url}. Escalation line {private_phone}.",
    "Beneficiary change form. Policyholder {private_person}, born {private_date}, payout account {account_number}, mailing address {private_address}, phone {private_phone}.",
    "HR onboarding: {private_person} starts on {private_date}. Corporate mailbox provisioned as {private_email}, payroll account {account_number}. Complete the record at {private_url}.",
    "Complaint case A-2291: {private_person} reports the parcel never arrived. Delivery address {private_address}, callback number {private_phone}. Case notes at {private_url}.",
    "Appointment reminder for {private_person}: your clinic visit is scheduled for {private_date}. To reschedule call {private_phone} or email {private_email}. The clinic is at {private_address}.",
    "Statement delivery settings. Account holder {private_person}, account {account_number}, postal address {private_address}, electronic statements to {private_email}.",
    "Handover memo: outgoing owner {private_person}, handover date {private_date}. The API key is {secret}, admin console at {private_url}, emergency contact {private_phone}.",
    "Lease summary: tenant {private_person}, identity verified on {private_date}, premises at {private_address}, deposit refund account {account_number}, phone {private_phone}.",
    "Reservation confirmed for {private_person} on {private_date}. Venue: {private_address}. To cancel, email {private_email} or call {private_phone}. Manage online at {private_url}.",
];

fn render(
    template: &str,
    i: usize,
    pools: &HashMap<&'static str, Vec<&'static str>>,
) -> Sample {
    let mut text = String::new();
    let mut gt = Vec::new();
    let mut rest = template;
    let mut salt = 0usize;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').map(|k| open + k).expect("unclosed {");
        text.push_str(&rest[..open]);
        let cat = &rest[open + 1..close];
        let pool = pools.get(cat).unwrap_or_else(|| panic!("no pool for {cat}"));
        // Deterministic per (sample index, placeholder position) selection.
        let v = pool[(i * 7 + salt * 3 + 1) % pool.len()];
        let s = text.len();
        text.push_str(v);
        gt.push((cat.to_string(), s, text.len()));
        salt += 1;
        rest = &rest[close + 1..];
    }
    text.push_str(rest);
    Sample { text, gt }
}

fn build_samples(templates: &[&str], pools: &HashMap<&'static str, Vec<&'static str>>, n: usize) -> Vec<Sample> {
    (0..n).map(|i| render(templates[i % templates.len()], i, pools)).collect()
}

fn build_long_docs(n: usize) -> Vec<Sample> {
    let pz = pools_zh();
    let pe = pools_en();
    let mut docs = Vec::new();
    for d in 0..n {
        let mut text = String::new();
        let mut gt: Vec<(String, usize, usize)> = Vec::new();
        let mut k = d * 13;
        while text.chars().count() < 3000 {
            let (tpl, pools): (&str, &HashMap<&'static str, Vec<&'static str>>) = if k % 2 == 0 {
                (TPL_ZH[k % TPL_ZH.len()], &pz)
            } else {
                (TPL_EN[k % TPL_EN.len()], &pe)
            };
            let s = render(tpl, k, pools);
            let base = text.len();
            text.push_str(&s.text);
            text.push('\n');
            for (c, a, b) in s.gt {
                gt.push((c, base + a, base + b));
            }
            k += 1;
        }
        docs.push(Sample { text, gt });
    }
    docs
}

// ---------------------------------------------------------------------------
// Inference
// ---------------------------------------------------------------------------

struct Decoded {
    viterbi_spans: Vec<Span>,
    argmax_spans: Vec<Span>,
    tokens: usize,
    disagreements: usize,
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    session: &mut Session,
    tokenizer: &Tokenizer,
    input_names: &[String],
    labels: &[LabelInfo],
    trans: &[Vec<bool>],
    text: &str,
    add_special: bool,
) -> Result<(Decoded, f64, f64), Box<dyn std::error::Error>> {
    let tok_t0 = Instant::now();
    let enc = tokenizer.encode(text, add_special).map_err(|e| format!("tokenize: {e}"))?;
    let tokenize_ms = tok_t0.elapsed().as_secs_f64() * 1000.0;
    let ids: Vec<i64> = enc.get_ids().iter().map(|&v| v as i64).collect();
    let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&v| v as i64).collect();
    let offsets: Vec<(usize, usize)> = enc.get_offsets().to_vec();
    let t = ids.len();
    if t == 0 {
        return Err("empty tokenization".into());
    }

    let t0 = Instant::now();
    let mut feeds: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = Vec::new();
    for name in input_names {
        let v: Vec<i64> = match name.as_str() {
            "input_ids" => ids.clone(),
            "attention_mask" => mask.clone(),
            "position_ids" => (0..t as i64).collect(),
            "token_type_ids" => vec![0i64; t],
            other => return Err(format!("unhandled model input `{other}`").into()),
        };
        let tensor = Tensor::from_array((vec![1i64, t as i64], v))?;
        feeds.push((Cow::Owned(name.clone()), SessionInputValue::from(tensor)));
    }

    let outputs = session.run(feeds)?;
    let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let n_lab = labels.len();
    if shape.len() != 3 || shape[1] as usize != t || shape[2] as usize != n_lab {
        return Err(format!(
            "unexpected logits shape {:?} (expected [1,{t},{n_lab}])",
            &shape[..]
        )
        .into());
    }

    let mut emis: Vec<Vec<f32>> = Vec::with_capacity(t);
    for i in 0..t {
        emis.push(log_softmax_row(&data[i * n_lab..(i + 1) * n_lab]));
    }

    let v_path = viterbi(&emis, labels, trans);
    let a_path: Vec<usize> = emis
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(0)
        })
        .collect();
    let disagreements = v_path.iter().zip(&a_path).filter(|(a, b)| a != b).count();

    Ok((
        Decoded {
            viterbi_spans: path_to_spans(&v_path, labels, &offsets, text),
            argmax_spans: path_to_spans(&a_path, labels, &offsets, text),
            tokens: t,
            disagreements,
        },
        tokenize_ms,
        elapsed_ms,
    ))
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(&home).join(".duduclaw/models/privacy-filter");
    let model_file =
        std::env::var("OPF_MODEL").unwrap_or_else(|_| "onnx/model_q4.onnx".to_string());
    let model_path = dir.join(&model_file);

    println!("=== OpenAI Privacy Filter — Rust/ONNX Runtime spike ===");
    println!("model dir : {}", dir.display());
    println!("model file: {}", model_path.display());
    println!(
        "graph size: {} bytes",
        std::fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0)
    );

    // ---- config.json id2label ----
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let id2label: HashMap<usize, String> = cfg["id2label"]
        .as_object()
        .ok_or("config.json has no id2label")?
        .iter()
        .map(|(k, v)| (k.parse::<usize>().unwrap(), v.as_str().unwrap().to_string()))
        .collect();
    let labels = parse_labels(&id2label);
    let trans = build_transitions(&labels);
    println!("labels    : {} ({} entity types)", labels.len(), (labels.len() - 1) / 4);

    // ---- tokenizer ----
    let tok_t0 = Instant::now();
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| format!("tokenizer load: {e}"))?;
    println!("tokenizer load: {:.0} ms", tok_t0.elapsed().as_secs_f64() * 1000.0);

    // special-token probe
    {
        let probe = "客戶王小明的電話是 0912-345-678";
        let a = tokenizer.encode(probe, false).map_err(|e| e.to_string())?;
        let b = tokenizer.encode(probe, true).map_err(|e| e.to_string())?;
        println!(
            "special-token probe: add_special=false -> {} tokens, add_special=true -> {} tokens",
            a.get_ids().len(),
            b.get_ids().len()
        );
        print!("first 12 tokens (offset-slice check): ");
        for i in 0..a.get_ids().len().min(12) {
            let (s, e) = a.get_offsets()[i];
            print!("[{:?}@{}..{}={:?}] ", a.get_tokens()[i], s, e, safe_slice(probe, s, e));
        }
        println!();
    }

    // ---- session ----
    let threads = std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2);
    let rss_before = rss_kb();
    let load_t0 = Instant::now();
    let mut session = Session::builder()?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
        .with_intra_threads(threads)?
        .commit_from_file(&model_path)?;
    let load_ms = load_t0.elapsed().as_secs_f64() * 1000.0;
    let rss_after = rss_kb();
    println!(
        "session load: {load_ms:.0} ms  intra_threads={threads}  RSS {} -> {} MB",
        rss_before.map(|v| v / 1024).unwrap_or(0),
        rss_after.map(|v| v / 1024).unwrap_or(0)
    );

    println!("--- session inputs ---");
    let mut input_names = Vec::new();
    for i in session.inputs() {
        println!("  {:<18} {:?}", i.name(), i.dtype());
        input_names.push(i.name().to_string());
    }
    println!("--- session outputs ---");
    for o in session.outputs() {
        println!("  {:<18} {:?}", o.name(), o.dtype());
    }

    // ---- smoke test ----
    let smoke = "客戶王小明於1990年3月5日來電，電話 0912-345-678，信箱 test@example.com.tw。";
    match run_one(&mut session, &tokenizer, &input_names, &labels, &trans, smoke, false) {
        Ok((d, tk, ms)) => {
            println!("\n[smoke] tokenize {tk:.1} ms + infer {ms:.1} ms / {} tokens", d.tokens);
            for s in &d.viterbi_spans {
                println!("        {:<16} {:?}", s.label, s.text);
            }
        }
        Err(e) => {
            println!("\n[smoke] FAILED: {e}");
            return Err(e);
        }
    }

    // ---- data ----
    let pz = pools_zh();
    let pe = pools_en();
    let zh: Vec<Sample> = build_samples(&TPL_ZH, &pz, 100);
    let en: Vec<Sample> = build_samples(&TPL_EN, &pe, 100);
    let longs = build_long_docs(5);
    println!(
        "\nsamples: zh-TW {} (chars {}..{}), en {} (chars {}..{}), long docs {} (avg {} chars)",
        zh.len(),
        zh.iter().map(|s| s.text.chars().count()).min().unwrap(),
        zh.iter().map(|s| s.text.chars().count()).max().unwrap(),
        en.len(),
        en.iter().map(|s| s.text.chars().count()).min().unwrap(),
        en.iter().map(|s| s.text.chars().count()).max().unwrap(),
        longs.len(),
        longs.iter().map(|s| s.text.chars().count()).sum::<usize>() / longs.len()
    );

    // ---- run plan (this export is slow on CPU; allow bounding the sweep) ----
    let lim = |k: &str, d: usize| {
        std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(d)
    };
    let n_zh = lim("OPF_N_ZH", 100).min(zh.len());
    let n_en = lim("OPF_N_EN", 100).min(en.len());
    let n_long = lim("OPF_N_LONG", 5).min(longs.len());
    let dump_zh = lim("OPF_DUMP_ZH", 25);
    let warm = lim("OPF_WARMUP", 3);
    println!("run plan: zh={n_zh} en={n_en} long={n_long} warmup={warm} (env OPF_N_ZH/OPF_N_EN/OPF_N_LONG/OPF_WARMUP)");
    flush();

    // ---- warm-up ----
    for i in 0..warm {
        let (d, tk, ms) = run_one(&mut session, &tokenizer, &input_names, &labels, &trans, &zh[0].text, false)?;
        println!("[warmup {}] {} tokens -> tokenize {tk:.0} ms + infer {ms:.0} ms", i + 1, d.tokens);
        flush();
    }

    let mut tok_total = 0usize;
    let mut disagree_total = 0usize;
    let mut span_sets_equal = 0usize;
    let mut span_sets_total = 0usize;

    // ---- zh-TW runs (streamed) ----
    println!("\n================ zh-TW RUNS ================");
    let mut lat_zh: Vec<f64> = Vec::new();
    let mut tok_zh: Vec<f64> = Vec::new();
    let mut zh_results: Vec<Vec<Span>> = Vec::new();
    for (i, s) in zh.iter().take(n_zh).enumerate() {
        let iter_t0 = Instant::now();
        let (d, tk, ms) = run_one(&mut session, &tokenizer, &input_names, &labels, &trans, &s.text, false)?;
        lat_zh.push(ms);
        tok_zh.push(tk);
        tok_total += d.tokens;
        disagree_total += d.disagreements;
        span_sets_total += 1;
        let same = fmt_spans(&d.viterbi_spans) == fmt_spans(&d.argmax_spans);
        if same {
            span_sets_equal += 1;
        }
        println!(
            "\n[zh {i}] {} chars / {} tokens -> tokenize {tk:.0} ms + infer {ms:.0} ms (iter wall {:.0} ms) | viterbi==argmax: {} | token-disagree {}",
            s.text.chars().count(),
            d.tokens,
            iter_t0.elapsed().as_secs_f64() * 1000.0,
            same,
            d.disagreements
        );
        if i < dump_zh {
            println!("  TEXT {}", s.text);
            for (c, a, b) in &s.gt {
                println!("  GT  {:<16} {:?}", c, safe_slice(&s.text, *a, *b));
            }
            if d.viterbi_spans.is_empty() {
                println!("  DET (none)");
            }
            for sp in &d.viterbi_spans {
                println!("  DET {:<16} {:?}", sp.label, sp.text);
            }
            if !same {
                println!("  ARGMAX-SPANS:");
                for sp in &d.argmax_spans {
                    println!("    AM  {:<16} {:?}", sp.label, sp.text);
                }
            }
        }
        zh_results.push(d.viterbi_spans);
        flush();
    }

    // ---- EN runs (streamed) ----
    println!("\n================ EN RUNS ================");
    let mut lat_en: Vec<f64> = Vec::new();
    let mut tok_en: Vec<f64> = Vec::new();
    let mut en_results: Vec<Vec<Span>> = Vec::new();
    for (i, s) in en.iter().take(n_en).enumerate() {
        let (d, tk, ms) = run_one(&mut session, &tokenizer, &input_names, &labels, &trans, &s.text, false)?;
        lat_en.push(ms);
        tok_en.push(tk);
        tok_total += d.tokens;
        disagree_total += d.disagreements;
        span_sets_total += 1;
        if fmt_spans(&d.viterbi_spans) == fmt_spans(&d.argmax_spans) {
            span_sets_equal += 1;
        }
        println!(
            "[en {i}] {} chars / {} tokens -> tokenize {tk:.0} ms + infer {ms:.0} ms | {} spans",
            s.text.chars().count(),
            d.tokens,
            d.viterbi_spans.len()
        );
        en_results.push(d.viterbi_spans);
        flush();
    }

    // ---- latency summary (short) ----
    let mut all: Vec<f64> = lat_zh.iter().chain(lat_en.iter()).cloned().collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut z = lat_zh.clone();
    z.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut e = lat_en.clone();
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n================ LATENCY (short samples, {} runs) ================", all.len());
    println!("intra_threads={threads}  total tokens scored={tok_total}");
    if !all.is_empty() {
        println!(
            "all   p50={:.0} ms  p95={:.0} ms  min={:.0}  max={:.0}",
            percentile(&all, 0.50),
            percentile(&all, 0.95),
            all[0],
            all[all.len() - 1]
        );
    }
    if !z.is_empty() {
        println!(
            "zh-TW n={} p50={:.0} ms  p95={:.0} ms",
            z.len(),
            percentile(&z, 0.50),
            percentile(&z, 0.95)
        );
    }
    if !e.is_empty() {
        println!(
            "en    n={} p50={:.0} ms  p95={:.0} ms",
            e.len(),
            percentile(&e, 0.50),
            percentile(&e, 0.95)
        );
    }
    let mut tk_all: Vec<f64> = tok_zh.iter().chain(tok_en.iter()).cloned().collect();
    tk_all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !tk_all.is_empty() {
        println!(
            "TOKENIZE (HF tokenizers, fancy-regex backend) p50={:.0} ms  p95={:.0} ms  max={:.0} ms",
            percentile(&tk_all, 0.50),
            percentile(&tk_all, 0.95),
            tk_all[tk_all.len() - 1]
        );
    }
    flush();

    // ---- long docs ----
    println!("\n================ LATENCY (long docs ~3000 chars) ================");
    let mut per_1k = Vec::new();
    let mut long_tok: Vec<f64> = Vec::new();
    let mut long_results = Vec::new();
    for (i, s) in longs.iter().take(n_long).enumerate() {
        let (d, tk, ms) = run_one(&mut session, &tokenizer, &input_names, &labels, &trans, &s.text, false)?;
        let chars = s.text.chars().count();
        long_tok.push(tk);
        let p = ms / (chars as f64 / 1000.0);
        per_1k.push(p);
        disagree_total += d.disagreements;
        tok_total += d.tokens;
        println!(
            "  doc{i}: {chars} chars / {} tokens -> tokenize {tk:.0} ms + infer {ms:.0} ms ({p:.0} ms infer per 1K chars), {} spans",
            d.tokens,
            d.viterbi_spans.len()
        );
        long_results.push(d.viterbi_spans);
        flush();
    }
    let mut p1 = per_1k.clone();
    p1.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !p1.is_empty() {
        println!("  median: {:.0} ms INFER per 1,000 chars", percentile(&p1, 0.5));
        let mut lt = long_tok.clone();
        lt.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("  median tokenize for long docs: {:.0} ms", percentile(&lt, 0.5));
    }

    println!("\nRSS after all runs: {} MB", rss_kb().map(|v| v / 1024).unwrap_or(0));

    // ---- Viterbi vs argmax ----
    println!("\n================ VITERBI vs ARGMAX ================");
    println!(
        "token-level disagreement: {}/{} = {:.4}%",
        disagree_total,
        tok_total,
        100.0 * disagree_total as f64 / tok_total.max(1) as f64
    );
    println!(
        "span-set identical on {}/{} short samples ({:.1}%)",
        span_sets_equal,
        span_sets_total,
        100.0 * span_sets_equal as f64 / span_sets_total.max(1) as f64
    );

    // ---- recall / FP tables ----
    println!("\n================ zh-TW QUALITY ================");
    print_quality(&zh[..n_zh], &zh_results);
    println!("\n================ EN QUALITY (reference) ================");
    print_quality(&en[..n_en], &en_results);
    println!("\n================ LONG-DOC QUALITY (mixed zh+en) ================");
    print_quality(&longs[..n_long], &long_results);

    Ok(())
}

fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn fmt_spans(v: &[Span]) -> Vec<(String, usize, usize)> {
    v.iter().map(|s| (s.label.clone(), s.start, s.end)).collect()
}

fn print_quality(samples: &[Sample], results: &[Vec<Span>]) {
    let mut gt_count: HashMap<&str, usize> = HashMap::new();
    let mut hit_count: HashMap<&str, usize> = HashMap::new();
    let mut hit_any_label: HashMap<&str, usize> = HashMap::new();
    let mut det_count: HashMap<String, usize> = HashMap::new();
    let mut fp = 0usize;
    let mut det_total = 0usize;
    let mut fp_examples: Vec<String> = Vec::new();

    for (s, dets) in samples.iter().zip(results) {
        for (cat, a, b) in &s.gt {
            let key = CATS.iter().find(|c| *c == cat).copied().unwrap_or("?");
            *gt_count.entry(key).or_default() += 1;
            let same = dets.iter().any(|d| d.label == *cat && overlaps((d.start, d.end), (*a, *b)));
            let any = dets.iter().any(|d| overlaps((d.start, d.end), (*a, *b)));
            if same {
                *hit_count.entry(key).or_default() += 1;
            }
            if any {
                *hit_any_label.entry(key).or_default() += 1;
            }
        }
        for d in dets {
            det_total += 1;
            *det_count.entry(d.label.clone()).or_default() += 1;
            let touches = s.gt.iter().any(|(_, a, b)| overlaps((d.start, d.end), (*a, *b)));
            if !touches {
                fp += 1;
                if fp_examples.len() < 15 {
                    fp_examples.push(format!("{}={:?}", d.label, d.text));
                }
            }
        }
    }

    println!(
        "{:<18} {:>6} {:>7} {:>9} {:>9} {:>9}",
        "category", "GT", "det", "hit(lbl)", "recall", "recall*"
    );
    let mut tot_gt = 0;
    let mut tot_hit = 0;
    let mut tot_any = 0;
    for c in CATS {
        let g = *gt_count.get(c).unwrap_or(&0);
        let h = *hit_count.get(c).unwrap_or(&0);
        let a = *hit_any_label.get(c).unwrap_or(&0);
        let d = *det_count.get(c).unwrap_or(&0);
        tot_gt += g;
        tot_hit += h;
        tot_any += a;
        println!(
            "{:<18} {:>6} {:>7} {:>9} {:>8.1}% {:>8.1}%",
            c,
            g,
            d,
            h,
            if g > 0 { 100.0 * h as f64 / g as f64 } else { 0.0 },
            if g > 0 { 100.0 * a as f64 / g as f64 } else { 0.0 }
        );
    }
    println!(
        "{:<18} {:>6} {:>7} {:>9} {:>8.1}% {:>8.1}%",
        "TOTAL",
        tot_gt,
        det_total,
        tot_hit,
        if tot_gt > 0 { 100.0 * tot_hit as f64 / tot_gt as f64 } else { 0.0 },
        if tot_gt > 0 { 100.0 * tot_any as f64 / tot_gt as f64 } else { 0.0 }
    );
    println!("  recall  = detected span with the SAME label overlaps ground truth");
    println!("  recall* = detected span with ANY label overlaps ground truth (label-agnostic)");
    println!(
        "false positives (detected span overlapping no ground truth): {fp} / {det_total} detections"
    );
    if !fp_examples.is_empty() {
        println!("  FP examples: {}", fp_examples.join(" | "));
    }
}
