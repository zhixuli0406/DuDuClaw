//! Zip entry-name hygiene for imported packs: junk filtering and filename
//! decoding.
//!
//! The ZIP format historically only knew IBM code page 437. General-purpose
//! bit 11 (EFS) says the name is UTF-8, and the Info-ZIP Unicode Path extra
//! field (0x7075) carries a UTF-8 copy; the `zip` crate already promotes a
//! valid 0x7075 field into the raw name and sets bit 11, so both arrive here
//! as "flagged". Everything else is a guess, and a guess must never land on
//! disk as mojibake (`µê░τòÑ…` for 「戰略…」). The order is:
//!
//! 1. flagged → must be valid UTF-8, else the archive is refused;
//! 2. unflagged, pure ASCII → as is;
//! 3. unflagged non-ASCII → one encoding for the whole archive (a single tool
//!    wrote it): UTF-8 when every such name is valid UTF-8 (macOS Archive
//!    Utility omits the flag), else GBK or Big5 — the one that decodes and
//!    round-trips every name, and when both do, the one whose text reads like
//!    real Chinese (more common characters); a tie is refused;
//! 4. nothing fits → refused with a zh-TW message and the raw bytes.
//!
//! Junk (`__MACOSX/`, `._*` AppleDouble files, `.DS_Store`, `Thumbs.db`,
//! `desktop.ini`) is recognised on the raw bytes, before decoding: `/` (0x2F)
//! never occurs inside a GBK or Big5 double-byte character, so splitting raw
//! names on it is safe. (`\` can, which is why a backslash is only treated as
//! a separator after decoding.)

/// A legacy (non-UTF-8) filename encoding chosen for an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyEncoding {
    Gbk,
    Big5,
}

impl LegacyEncoding {
    pub fn label(self) -> &'static str {
        match self {
            LegacyEncoding::Gbk => "GBK（簡體中文 Windows）",
            LegacyEncoding::Big5 => "Big5（繁體中文 Windows）",
        }
    }

    fn encoding(self) -> &'static encoding_rs::Encoding {
        match self {
            LegacyEncoding::Gbk => encoding_rs::GBK,
            LegacyEncoding::Big5 => encoding_rs::BIG5,
        }
    }
}

/// One entry's raw name as the archive stores it.
#[derive(Debug, Clone)]
pub struct RawName {
    pub bytes: Vec<u8>,
    /// Bit 11 set (directly, or promoted from a valid 0x7075 field).
    pub utf8_flag: bool,
}

/// Decoded names, index-aligned with the input, plus the legacy encoding
/// that was needed (if any).
#[derive(Debug, Clone)]
pub struct DecodedNames {
    pub names: Vec<String>,
    pub legacy: Option<LegacyEncoding>,
}

/// Is this raw entry name OS clutter that must never be extracted?
pub fn is_junk_raw(raw: &[u8]) -> bool {
    let parts: Vec<&[u8]> = raw
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty())
        .collect();
    if parts.iter().any(|p| *p == b"__MACOSX") {
        return true;
    }
    match parts.last() {
        Some(last) => is_junk_basename(last),
        None => false,
    }
}

/// Junk check on one decoded or raw basename.
pub fn is_junk_basename(name: &[u8]) -> bool {
    name.starts_with(b"._")
        || name == b".DS_Store"
        || name.eq_ignore_ascii_case(b"Thumbs.db")
        || name.eq_ignore_ascii_case(b"desktop.ini")
        || name == b"__MACOSX"
}

/// Decode every name of one archive (see the module docs for the order).
/// Junk entries should be filtered out by the caller first; they would only
/// add noise to the encoding vote.
pub fn decode_names(raw: &[RawName]) -> Result<DecodedNames, String> {
    // Unflagged names that need a guess.
    let guess_idx: Vec<usize> = raw
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.utf8_flag && !r.bytes.is_ascii())
        .map(|(i, _)| i)
        .collect();
    let all_utf8 = guess_idx
        .iter()
        .all(|&i| std::str::from_utf8(&raw[i].bytes).is_ok());
    let legacy = if guess_idx.is_empty() || all_utf8 {
        None
    } else {
        Some(pick_legacy(raw, &guess_idx)?)
    };

    let mut names = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        let name = match legacy {
            Some(enc) if !r.utf8_flag && !r.bytes.is_ascii() => {
                decode_strict(enc, &r.bytes).ok_or_else(|| undecodable_error(i, &r.bytes))?
            }
            _ => String::from_utf8(r.bytes.clone()).map_err(|_| undecodable_error(i, &r.bytes))?,
        };
        names.push(name);
    }
    Ok(DecodedNames { names, legacy })
}

/// Choose GBK or Big5 for the guessed names. Fail closed on no fit or a tie.
fn pick_legacy(raw: &[RawName], guess_idx: &[usize]) -> Result<LegacyEncoding, String> {
    let score = |enc: LegacyEncoding| -> Option<i64> {
        let mut total = 0i64;
        for &i in guess_idx {
            let text = decode_strict(enc, &raw[i].bytes)?;
            total += plausibility(&text);
        }
        Some(total)
    };
    let gbk = score(LegacyEncoding::Gbk);
    let big5 = score(LegacyEncoding::Big5);
    match (gbk, big5) {
        (Some(_), None) => Ok(LegacyEncoding::Gbk),
        (None, Some(_)) => Ok(LegacyEncoding::Big5),
        (Some(g), Some(b)) if g > b => Ok(LegacyEncoding::Gbk),
        (Some(g), Some(b)) if b > g => Ok(LegacyEncoding::Big5),
        (Some(_), Some(_)) => Err(format!(
            "無法判斷壓縮檔內的中文檔名是 Big5 還是 GBK 編碼（例如 {}），為避免寫出亂碼已停止匯入。\
             請在原電腦用支援 UTF-8 的壓縮工具（例如 macOS 內建壓縮、7-Zip 勾選 UTF-8）重新壓縮後再上傳。",
            escape_bytes(&raw[guess_idx[0]].bytes)
        )),
        (None, None) => Err(undecodable_error(guess_idx[0], &raw[guess_idx[0]].bytes)),
    }
}

/// Decode `bytes` with `enc`, refusing replacement characters and anything
/// that does not encode back to the same bytes.
fn decode_strict(enc: LegacyEncoding, bytes: &[u8]) -> Option<String> {
    let text = enc
        .encoding()
        .decode_without_bom_handling_and_without_replacement(bytes)?
        .into_owned();
    let (back, _, had_errors) = enc.encoding().encode(&text);
    if had_errors || back.as_ref() != bytes {
        return None;
    }
    Some(text)
}

/// How much `text` reads like ordinary Chinese: common characters score,
/// characters from rarely used blocks cost. Mis-decoded bytes land on rare
/// characters far more often than on the few hundred most frequent ones.
fn plausibility(text: &str) -> i64 {
    text.chars()
        .map(|c| {
            if c.is_ascii() {
                0
            } else if COMMON_HAN.contains(c) {
                3
            } else if ('\u{4E00}'..='\u{9FFF}').contains(&c) || is_cjk_punct(c) {
                0
            } else {
                -2
            }
        })
        .sum()
}

fn is_cjk_punct(c: char) -> bool {
    ('\u{3000}'..='\u{303F}').contains(&c) || ('\u{FF00}'..='\u{FFEF}').contains(&c)
}

fn undecodable_error(index: usize, bytes: &[u8]) -> String {
    format!(
        "壓縮檔第 {} 個項目的檔名無法辨識編碼（原始內容：{}），為避免寫出亂碼已停止匯入。\
         請用支援 UTF-8 檔名的壓縮工具重新壓縮後再上傳。",
        index + 1,
        escape_bytes(bytes)
    )
}

/// ASCII shown as is, everything else as `\xNN` — never raw bytes in a message.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes.iter().take(80) {
        if b.is_ascii_graphic() || *b == b' ' {
            out.push(*b as char);
        } else {
            out.push_str(&format!("\\x{b:02X}"));
        }
    }
    if bytes.len() > 80 {
        out.push('…');
    }
    out
}

/// The most frequent Han characters in everyday Traditional and Simplified
/// Chinese (both scripts mixed; duplicates are harmless). Only used to break a
/// GBK/Big5 tie, so a few hundred is plenty.
const COMMON_HAN: &str = "的一是不了人我在有他這个這個们們中来來上大为為和国國地到以说說时時要就出会會可也你对對生能而子那得于於着著下自之年过過发發后後作里裡用道行所然家种種事成方多经經么麼去法学學如都同现現当當没沒动動面起看定天分还還进進好小部其些主样樣理心她本前开開但因只从從想实實日军軍者意无無力它与與长長把机機十民第公此已工使情明性知全三又关關点點正业業外将將两兩高间間由问問很最重并並物手应應战戰向头頭文体體政美相见見被利什二等产產或新己制身果加西斯月话話合回特代内內信表化老给給世位次度门門任常先海通教儿兒原东東声聲提立及比员員解水名真论論处處走义義各入几幾口认認条條平系气氣题題活尔爾更别別打女变變四神总總何电電数數安少报報才结結反受目太量再感建务務做接必场場件计計管期市直德资資命山金指克许許统統区區保至队隊形社便空决決治展马馬科司五基眼书書非则則听聽白却卻界达達光放强強即像难難且权權思王象完设設式色路记記南品住告类類求据據程北边邊死张張该該交规規万萬取拉格望觉覺术術领領共确確传傳师師观觀清今切院让讓识識候带帶导導争爭运運笑飞飛风風步改收根干造言联聯持组組每济濟车車亲親极極林服快办辦议議往元英士证證近失转轉夫令准準布始怎存未远遠叫台单單影具罗羅字爱愛击擊流备備兵连連调調深商算质質团團集百需价價花党黨华華城石级級整府离離况況亚亞请請技际際约約示复復病息究线線似官火断斷精满滿支视視消越器容照须須九增研写寫称稱企八功包片史委乎查轻輕易早曾除农農找装裝广廣显顯图圖念六引历歷首医醫局突专專费費号號尽盡另周较較注语語仅僅考落青随隨选選列武红紅响響虽雖推势勢参參希古众眾构構房半节節土投某案黑维維革划劃敌敵致陈陳律足态態护護七兴興派孩验驗责責营營星够夠章音跟志底站严嚴巴例防族供效续續施留讲講型料终終答紧緊黄黃绝絕奇察母京段依批群项項故按河米围圍江织織害斗双雙境客纪紀采举舉杀殺攻父苏蘇密低朝友诉訴止细細愿願千值仍男钱錢破网網热熱助倒育属屬坐帝限船脸臉职職速刻乐樂否刚剛威毛状狀率甚独獨球般普怕弹彈校苦创創假久错錯承印晚兰蘭试試股拿脑腦预預谁誰益阳陽若微尼继繼送急血惊驚伤傷素药藥适適波夜省初喜卫衛源食险險待述陆陸习習置居劳勞财財环環排福纳納欢歡雷警获獲模充负負云雲停木游龙龍树樹疑层層冷洲冲衝射略范範竟句室异異激汉漢村策演简簡卡罪判担擔州静靜退既衣您宗积積余餘痛检檢差富灵靈协協角占配征修皮挥揮胜勝降阶階审審沉坚堅善读讀超免压壓银銀买買皇养養伊怀懷执執副乱亂抗犯追帮幫宣佛岁歲航优優怪香田铁鐵控税稅左右份穿艺藝背阵陣草脚腳概恶惡块塊顿頓敢守酒岛島托央户戶烈洋哥索胡款靠评評版宝寶座释釋景顾顧弟登货貨互付伯慢欧歐换換闻聞危忙核暗姐介坏壞讨討丽麗良序升监監临臨亮露永呼味野架域沙掉括舰艦鱼魚杂雜误誤湖钢鋼颜顏毒报報告顾顧问問略战戰技能档檔案件资資料夹夾文件图圖片表格说說明手册冊会會议議记記录錄销銷售财財务務客户戶服务務产產品分析计計划劃总總结結周月年度季版本草稿备備份新建项項目合同发發票简簡历歷照片视視频頻音乐樂下载載桌面";

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(bytes: Vec<u8>, utf8_flag: bool) -> RawName {
        RawName { bytes, utf8_flag }
    }

    fn enc(e: &'static encoding_rs::Encoding, s: &str) -> Vec<u8> {
        let (b, _, err) = e.encode(s);
        assert!(!err, "test string not representable");
        b.into_owned()
    }

    #[test]
    fn junk_is_recognised_on_raw_bytes() {
        assert!(is_junk_raw(b"__MACOSX/x/._a"));
        assert!(is_junk_raw(b"pack/._SKILL.md"));
        assert!(is_junk_raw(b"pack/.DS_Store"));
        assert!(is_junk_raw(b"pack/THUMBS.DB"));
        assert!(is_junk_raw(b"desktop.ini"));
        assert!(!is_junk_raw(b"pack/SKILL.md"));
        assert!(!is_junk_raw(b"pack/.claude/agents/a.md"));
        // A GBK name never splits wrongly: no 0x2F inside a double-byte char.
        let gbk = enc(encoding_rs::GBK, "战略/技能.md");
        assert!(!is_junk_raw(&gbk));
    }

    #[test]
    fn flagged_names_must_be_utf8() {
        let ok = decode_names(&[raw("戰略/a.md".as_bytes().to_vec(), true)]).unwrap();
        assert_eq!(ok.names[0], "戰略/a.md");
        assert!(ok.legacy.is_none());
        let bad = decode_names(&[raw(vec![0xB5, 0xD4, b'/'], true)]);
        assert!(bad.is_err());
    }

    /// macOS Archive Utility writes UTF-8 bytes without bit 11 — the
    /// distributor's sample is exactly this; it used to come out as CP437.
    #[test]
    fn unflagged_utf8_is_read_as_utf8_not_cp437() {
        let names = vec![
            raw("報告顧問/".as_bytes().to_vec(), false),
            raw("報告顧問/manifest.json".as_bytes().to_vec(), false),
        ];
        let d = decode_names(&names).unwrap();
        assert_eq!(d.names[0], "報告顧問/");
        assert!(d.legacy.is_none());
    }

    #[test]
    fn big5_archive_from_traditional_windows_is_decoded() {
        let names = vec![
            raw(enc(encoding_rs::BIG5, "報告顧問/"), false),
            raw(enc(encoding_rs::BIG5, "報告顧問/技能說明.md"), false),
            raw(b"README.md".to_vec(), false),
        ];
        let d = decode_names(&names).unwrap();
        assert_eq!(d.legacy, Some(LegacyEncoding::Big5));
        assert_eq!(d.names[1], "報告顧問/技能說明.md");
        assert_eq!(d.names[2], "README.md");
    }

    #[test]
    fn gbk_archive_from_simplified_windows_is_decoded() {
        let names = vec![
            raw(enc(encoding_rs::GBK, "报告顾问/"), false),
            raw(enc(encoding_rs::GBK, "报告顾问/技能说明.md"), false),
        ];
        let d = decode_names(&names).unwrap();
        assert_eq!(d.legacy, Some(LegacyEncoding::Gbk));
        assert_eq!(d.names[1], "报告顾问/技能说明.md");
    }

    /// CP437-only bytes (a Western "café" from an old DOS tool) are neither
    /// UTF-8 nor GBK/Big5 → refused, never written as mojibake.
    #[test]
    fn cp437_only_names_are_refused_with_a_clear_message() {
        let err = decode_names(&[raw(
            vec![b'c', b'a', b'f', 0x82, b'.', b't', b'x', b't'],
            false,
        )])
        .unwrap_err();
        assert!(err.contains("無法辨識編碼"), "{err}");
        assert!(err.contains("\\x82"), "{err}");
    }
}
