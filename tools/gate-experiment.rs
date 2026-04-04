#!/usr/bin/env -S cargo +nightly -Zscript
//! GATE: Simon Protocol — Embedding Pipeline Comparison Experiment
//!
//! Compares keyword-based topic_surprise (Pipeline A) vs vocabulary_novelty
//! (Pipeline B / Tier 2) using synthetic zh-TW conversations.
//!
//! Run: rust-script tools/gate-experiment.rs
//!   or: rustc tools/gate-experiment.rs -o /tmp/gate && /tmp/gate

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Simulated conversation
// ---------------------------------------------------------------------------

struct Conv {
    id: String,
    agent_id: String,
    user_id: String,
    user_messages: Vec<&'static str>,
    has_topic_shift: bool,
    language: &'static str,
    topic_category: &'static str,
}

struct Row {
    id: String,
    lang: String,
    category: String,
    has_shift: bool,
    kw_surprise: f64,
    kw_class: String,
    nov_surprise: f64,
    nov_class: String,
    changed: bool,
    is_tail: bool,
}

// ---------------------------------------------------------------------------
// Synthetic data — realistic zh-TW conversations
// ---------------------------------------------------------------------------

fn gen_convos() -> Vec<Conv> {
    let mut out = Vec::new();
    let mut id = 0u32;

    // Technical (50)
    let tech: Vec<Vec<&str>> = vec![
        vec!["請問 API 回傳 500 錯誤怎麼處理？", "我用的是 Python requests 套件"],
        vec!["Docker container 啟動失敗", "log 顯示 OOM killed 記憶體 512MB"],
        vec!["Git merge conflict 要怎麼解？", "兩個 branch 都改了同一個檔案"],
        vec!["Rust 的 lifetime 搞不懂", "borrow checker 不讓我編譯"],
        vec!["K8s pod CrashLoopBackOff", "readiness probe 失敗"],
        vec!["TypeScript generic 怎麼用？", "通用 API client"],
        vec!["PostgreSQL 查詢慢 seq scan", "一百萬筆資料"],
        vec!["React useEffect dependency", "component 一直 re-render"],
        vec!["nginx WebSocket 斷線 502", "反向代理設定"],
        vec!["GitHub Actions 部署 AWS", "CI/CD 自動化"],
    ];
    for msgs in &tech {
        for v in 0..5 {
            id += 1;
            out.push(Conv {
                id: format!("conv-{id:04}"), agent_id: "agent-tech".into(),
                user_id: format!("user-{}", id % 20),
                user_messages: msgs.clone(),
                has_topic_shift: false, language: "zh", topic_category: "technical",
            });
        }
    }

    // Casual (30)
    let casual: Vec<Vec<&str>> = vec![
        vec!["你好！今天天氣不錯", "推薦什麼好吃的？"],
        vec!["最近好累", "工作壓力好大"],
        vec!["週末去哪玩？", "想看台北展覽"],
        vec!["人生的意義是什麼？", "最近一直在想"],
        vec!["推薦好看電影", "喜歡科幻片"],
        vec!["台灣夜市哪個最好？", "想去逢甲"],
    ];
    for msgs in &casual {
        for _ in 0..5 {
            id += 1;
            out.push(Conv {
                id: format!("conv-{id:04}"), agent_id: "agent-general".into(),
                user_id: format!("user-{}", id % 20),
                user_messages: msgs.clone(),
                has_topic_shift: false, language: "zh", topic_category: "casual",
            });
        }
    }

    // Topic shifts (20)
    let shifts: Vec<Vec<&str>> = vec![
        vec!["幫我查帳單", "算了，我想問怎麼寫 Python"],
        vec!["今天心情不好", "對了你知道怎麼設定 SSL 嗎？"],
        vec!["推薦餐廳", "還有我程式有 bug 幫我看"],
        vec!["AI 會取代人類嗎？", "不說了幫我翻譯英文"],
    ];
    for msgs in &shifts {
        for _ in 0..5 {
            id += 1;
            out.push(Conv {
                id: format!("conv-{id:04}"), agent_id: "agent-general".into(),
                user_id: format!("user-{}", id % 20),
                user_messages: msgs.clone(),
                has_topic_shift: true, language: "zh", topic_category: "topic_shift",
            });
        }
    }

    // English (10)
    let en: Vec<Vec<&str>> = vec![
        vec!["How do I set up a REST API?", "Using Express.js"],
        vec!["Explain machine learning", "Specifically supervised learning"],
    ];
    for msgs in &en {
        for _ in 0..5 {
            id += 1;
            out.push(Conv {
                id: format!("conv-{id:04}"), agent_id: "agent-tech".into(),
                user_id: format!("user-{}", id % 20),
                user_messages: msgs.clone(),
                has_topic_shift: false, language: "en", topic_category: "technical_en",
            });
        }
    }

    // Edge cases (10)
    let edges: Vec<Vec<&str>> = vec![
        vec!["好"], vec!["？？？"], vec!["```fn main() {}```"],
        vec!["😊😊😊"], vec!["不是我的意思是那個算了"],
    ];
    for msgs in &edges {
        for _ in 0..2 {
            id += 1;
            out.push(Conv {
                id: format!("conv-{id:04}"), agent_id: "agent-general".into(),
                user_id: format!("user-{}", id % 20),
                user_messages: msgs.clone(),
                has_topic_shift: false, language: "zh", topic_category: "edge_case",
            });
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Pipeline A: keyword topic_surprise
// ---------------------------------------------------------------------------

fn is_cjk(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp) || (0x3400..=0x4DBF).contains(&cp)
}

fn extract_kw(text: &str) -> Vec<String> {
    let mut freq: HashMap<String, u32> = HashMap::new();
    let chars: Vec<char> = text.chars().collect();
    for w in chars.windows(2) {
        if is_cjk(w[0] as u32) && is_cjk(w[1] as u32) {
            *freq.entry(w.iter().collect()).or_insert(0) += 1;
        }
    }
    let stop = ["the","a","is","to","of","in","for","on","it","i","you","and","or","but"];
    for word in text.split_whitespace() {
        let l: String = word.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        if l.len() >= 2 && !stop.contains(&l.as_str()) && l.chars().all(|c| c.is_ascii_alphabetic()) {
            *freq.entry(l).or_insert(0) += 1;
        }
    }
    let mut e: Vec<_> = freq.into_iter().collect();
    e.sort_by(|a, b| b.1.cmp(&a.1));
    e.into_iter().take(5).map(|(k, _)| k).collect()
}

fn kw_surprise(cur: &[String], hist: &[String]) -> f64 {
    if hist.is_empty() || cur.is_empty() { return 0.0; }
    let a: HashSet<&String> = cur.iter().collect();
    let b: HashSet<&String> = hist.iter().collect();
    let i = a.intersection(&b).count() as f64;
    let u = a.union(&b).count() as f64;
    if u == 0.0 { 0.0 } else { (1.0 - i / u) * 0.7 }
}

// ---------------------------------------------------------------------------
// Pipeline B: vocabulary novelty
// ---------------------------------------------------------------------------

fn vocab_novelty(text: &str, hist: &HashSet<String>) -> f64 {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let cur: HashSet<String> = chars.windows(2).map(|w| w.iter().collect()).collect();
    if cur.is_empty() { return 0.0; }
    cur.difference(hist).count() as f64 / cur.len() as f64
}

// ---------------------------------------------------------------------------
// Composite error
// ---------------------------------------------------------------------------

fn classify(c: f64) -> &'static str {
    if c < 0.2 { "Negligible" } else if c < 0.5 { "Moderate" }
    else if c < 0.8 { "Significant" } else { "Critical" }
}

fn composite_a(ts: f64) -> f64 { (0.05 * ts).clamp(0.0, 1.0) } // topic only, others baseline
fn composite_b(ts: f64) -> f64 { (0.20 * ts).clamp(0.0, 1.0) }

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let convos = gen_convos();
    let total = convos.len();

    let mut kw_hist: HashMap<String, Vec<String>> = HashMap::new();
    let mut bg_hist: HashMap<String, HashSet<String>> = HashMap::new();
    let mut rows: Vec<Row> = Vec::new();

    for c in &convos {
        let text: String = c.user_messages.join(" ");
        let key = format!("{}:{}", c.user_id, c.agent_id);

        let cur_kw = extract_kw(&text);
        let h_kw = kw_hist.get(&key).cloned().unwrap_or_default();
        let ks = kw_surprise(&cur_kw, &h_kw);
        let ka = composite_a(ks);

        let h_bg = bg_hist.get(&key).cloned().unwrap_or_default();
        let ns = vocab_novelty(&text, &h_bg);
        let nb = composite_b(ns);

        let kc = classify(ka);
        let nc = classify(nb);

        // update history
        kw_hist.entry(key.clone()).or_default().extend(cur_kw);
        let bh = bg_hist.entry(key).or_default();
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        for w in chars.windows(2) { bh.insert(w.iter().collect()); }

        rows.push(Row {
            id: c.id.clone(), lang: c.language.into(), category: c.topic_category.into(),
            has_shift: c.has_topic_shift,
            kw_surprise: ks, kw_class: kc.into(),
            nov_surprise: ns, nov_class: nc.into(),
            changed: kc != nc, is_tail: false,
        });
    }

    // mark tail 5%
    let mut by_comp: Vec<(usize, f64)> = rows.iter().enumerate()
        .map(|(i, r)| (i, composite_a(r.kw_surprise).max(composite_b(r.nov_surprise))))
        .collect();
    by_comp.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let tc = (total as f64 * 0.05).ceil() as usize;
    for &(i, _) in by_comp.iter().take(tc) { rows[i].is_tail = true; }

    // ── Stats ──
    let changed = rows.iter().filter(|r| r.changed).count();
    let rate = changed as f64 / total as f64 * 100.0;

    let zh: Vec<&Row> = rows.iter().filter(|r| r.lang == "zh").collect();
    let zh_ch = zh.iter().filter(|r| r.changed).count();
    let zh_rate = zh_ch as f64 / zh.len() as f64 * 100.0;

    let normal: Vec<&Row> = rows.iter().filter(|r| !r.is_tail).collect();
    let tail: Vec<&Row> = rows.iter().filter(|r| r.is_tail).collect();
    let n_ch = normal.iter().filter(|r| r.changed).count();
    let t_ch = tail.iter().filter(|r| r.changed).count();
    let n_rate = n_ch as f64 / normal.len() as f64 * 100.0;
    let t_rate = if tail.is_empty() { 0.0 } else { t_ch as f64 / tail.len() as f64 * 100.0 };

    let shifts: Vec<&Row> = rows.iter().filter(|r| r.has_shift).collect();
    let shift_kw = shifts.iter().filter(|r| r.kw_surprise > 0.3).count();
    let shift_nv = shifts.iter().filter(|r| r.nov_surprise > 0.3).count();

    let mut by_cat: HashMap<String, (usize, usize)> = HashMap::new();
    for r in &rows {
        let e = by_cat.entry(r.category.clone()).or_default();
        e.0 += 1;
        if r.changed { e.1 += 1; }
    }

    // corrected thresholds (×0.6 Kahneman)
    let corr_all = rate / 0.6;
    let corr_tail = t_rate / 0.6;

    let decision = if corr_all < 3.0 && corr_tail < 10.0 {
        "KILL — 不加 embedding"
    } else if corr_all > 9.0 || corr_tail > 25.0 {
        "DEFAULT ON — embedding 預設啟用"
    } else {
        "OPTIONAL — embedding 作為可選功能"
    };

    // shift detection
    let s_total = shifts.len().max(1);
    let kw_detect = shift_kw as f64 / s_total as f64 * 100.0;
    let nv_detect = shift_nv as f64 / s_total as f64 * 100.0;

    // novelty validation
    let shift_avg: f64 = shifts.iter().map(|r| r.nov_surprise).sum::<f64>() / s_total as f64;
    let non_shift: Vec<&Row> = rows.iter().filter(|r| !r.has_shift && r.lang == "zh").collect();
    let ns_avg: f64 = non_shift.iter().map(|r| r.nov_surprise).sum::<f64>() / non_shift.len().max(1) as f64;

    println!();
    println!("══════════════════════════════════════════════════════════════");
    println!("  GATE: Simon Protocol — Pipeline Comparison Experiment");
    println!("  Synthetic data: {} conversations", total);
    println!("══════════════════════════════════════════════════════════════");
    println!();
    println!("┌──────────────────────────────────────────────────────────┐");
    println!("│ GATE-05: Decision Change Rate                           │");
    println!("│   Overall:  {:>3} / {:>3} = {:>5.1}%                       │", changed, total, rate);
    println!("│   zh-TW:    {:>3} / {:>3} = {:>5.1}%                       │", zh_ch, zh.len(), zh_rate);
    println!("├──────────────────────────────────────────────────────────┤");
    println!("│ GATE-06: Barbell Split (Taleb)                          │");
    println!("│   Normal ({:>3}):  {:>3} changed = {:>5.1}%                │", normal.len(), n_ch, n_rate);
    println!("│   Tail   ({:>3}):  {:>3} changed = {:>5.1}%                │", tail.len(), t_ch, t_rate);
    println!("├──────────────────────────────────────────────────────────┤");
    println!("│ Topic Shift Detection ({} ground truth)                │", shifts.len());
    println!("│   Keyword:   {:>3} / {:>3} = {:>5.1}%                      │", shift_kw, s_total, kw_detect);
    println!("│   Novelty:   {:>3} / {:>3} = {:>5.1}%                      │", shift_nv, s_total, nv_detect);
    println!("├──────────────────────────────────────────────────────────┤");
    println!("│ By Category:                                            │");
    let mut cats: Vec<_> = by_cat.iter().collect();
    cats.sort_by_key(|(k, _)| k.clone());
    for (cat, (t, c)) in &cats {
        println!("│   {:<15} {:>3} / {:>3} = {:>5.1}%                   │", cat, c, t, *c as f64 / *t as f64 * 100.0);
    }
    println!("├──────────────────────────────────────────────────────────┤");
    println!("│ Novelty Validation:                                     │");
    println!("│   Topic-shift avg novelty: {:.3}                        │", shift_avg);
    println!("│   Non-shift avg novelty:   {:.3}                        │", ns_avg);
    println!("│   Novelty separates shift: {}                         │",
        if shift_avg > ns_avg { "YES ✓" } else { "NO ✗ " });
    println!("├──────────────────────────────────────────────────────────┤");
    println!("│ GATE-10: Decision (×0.6 Kahneman correction)            │");
    println!("│   Corrected overall: {:>5.1}%                            │", corr_all);
    println!("│   Corrected tail:    {:>5.1}%                            │", corr_tail);
    println!("│                                                         │");
    println!("│   >>> {}  │", decision);
    println!("└──────────────────────────────────────────────────────────┘");
    println!();
    println!("⚠️  SYNTHETIC DATA — re-run with real prediction_log for go/no-go.");
    println!("   When ~/.duduclaw/prediction.db exists, this tool will read it.");
    println!();

    // CSV output
    let csv_path = "/tmp/gate-experiment-results.csv";
    let mut csv = String::from("id,language,category,has_shift,kw_surprise,kw_class,nov_surprise,nov_class,changed,is_tail\n");
    for r in &rows {
        csv.push_str(&format!("{},{},{},{},{:.4},{},{:.4},{},{},{}\n",
            r.id, r.lang, r.category, r.has_shift,
            r.kw_surprise, r.kw_class, r.nov_surprise, r.nov_class,
            r.changed, r.is_tail));
    }
    std::fs::write(csv_path, &csv).expect("Failed to write CSV");
    println!("📊 CSV written to: {csv_path}");
}
