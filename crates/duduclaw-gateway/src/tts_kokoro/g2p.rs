// Ported from hexgrad/misaki zh.py and zh_frontend.py (v1.1 path).
// Copyright hexgrad (misaki contributors).
// PaddleSpeech-derived frontend: Copyright (c) 2021 PaddlePaddle Authors.
// Licensed under Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0
// Modified: Rust, jieba-rs/pinyin, compact number and character normalization.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use jieba_rs::Jieba;
use pinyin::ToPinyin;
use super::{english, tone_sandhi::{self, Word}};

const ZH_MAP: &[(&str, &str)] = &[("b","ㄅ"),("p","ㄆ"),("m","ㄇ"),("f","ㄈ"),("d","ㄉ"),("t","ㄊ"),("n","ㄋ"),("l","ㄌ"),("g","ㄍ"),("k","ㄎ"),("h","ㄏ"),("j","ㄐ"),("q","ㄑ"),("x","ㄒ"),("zh","ㄓ"),("ch","ㄔ"),("sh","ㄕ"),("r","ㄖ"),("z","ㄗ"),("c","ㄘ"),("s","ㄙ"),("a","ㄚ"),("o","ㄛ"),("e","ㄜ"),("ie","ㄝ"),("ai","ㄞ"),("ei","ㄟ"),("ao","ㄠ"),("ou","ㄡ"),("an","ㄢ"),("en","ㄣ"),("ang","ㄤ"),("eng","ㄥ"),("er","ㄦ"),("i","ㄧ"),("u","ㄨ"),("v","ㄩ"),("ii","ㄭ"),("iii","十"),("ve","月"),("ia","压"),("ian","言"),("iang","阳"),("iao","要"),("in","阴"),("ing","应"),("iong","用"),("iou","又"),("ong","中"),("ua","穵"),("uai","外"),("uan","万"),("uang","王"),("uei","为"),("uen","文"),("ueng","瓮"),("uo","我"),("van","元"),("vn","云")];
const MUST_ERHUA: &[&str] = &["小院儿","胡同儿","范儿","老汉儿","撒欢儿","寻老礼儿","妥妥儿","媳妇儿"];
const NOT_ERHUA: &[&str] = &["虐儿","为儿","护儿","瞒儿","救儿","替儿","有儿","一儿","我儿","俺儿","妻儿","拐儿","聋儿","乞儿","患儿","幼儿","孤儿","婴儿","婴幼儿","连体儿","脑瘫儿","流浪儿","体弱儿","混血儿","蜜雪儿","舫儿","祖儿","美儿","应采儿","可儿","侄儿","孙儿","侄孙儿","女儿","男儿","红孩儿","花儿","虫儿","马儿","鸟儿","猪儿","猫儿","狗儿","少儿"];

static VOCAB: OnceLock<HashMap<char, i64>> = OnceLock::new();
static JIEBA: OnceLock<Jieba> = OnceLock::new();

pub(super) fn vocabulary() -> &'static HashMap<char, i64> {
    VOCAB.get_or_init(|| {
        let value: serde_json::Value = serde_json::from_str(include_str!("tokenizer.json"))
            .expect("bundled Kokoro vocabulary must be valid JSON");
        value["model"]["vocab"].as_object().expect("bundled vocabulary object").iter()
            .filter_map(|(k,v)| Some((k.chars().next()?, v.as_i64()?))).collect()
    })
}

/// Small, authored character substitutions; no external conversion data.
/// Ambiguous phrase conversions intentionally retain their character reading.
fn simplify(text: &str) -> String {
    const PAIRS: &str = "臺台灣湾設设員员氣气這这個个為为來来說说語语聲声電电腦脑開开關关閉闭網网頁页時时間间點点數数據据學学習习體体驗验測测試试應应該该請请問问題题會会議议與与過过還还沒没麼么樣样從从對对話话見见頭头發发後后裏里裡里國国產产業业務务實实現现執执行行處处理理資资訊讯檔档案案載载儲储存存權权限限風风險险錯错誤误選选擇择預预備备轉转換换連连線线裝装置置讓让導导覽览畫画圖图書书寫写讀读長长萬万億亿兩两幾几隻只隨随機机種种別别區区域域帳账號号碼码密密鑰钥報报告告單单簡简單单確确認认軟软體体啟启動动無无義义錄录製制乾干淨净隱隐私私標标準准專专屬属門门內内兒儿媽妈馬马愛爱難难聽听樂乐歡欢費费圍围歲岁錢钱東东條条雲云華华園园圓圆組组織织運运輸输傳传達达維维護护終终於于經经濟济優优質质務务廣广東东買买賣卖顯显示示復复製制遠远離离歡欢迎迎滅灭燈灯種种紅红綠绿藍蓝黃黄黑黑壞坏邊边隻只貓猫狗狗車车飛飞廠厂廳厅歷历壓压郵邮簡简親亲訪访醫医療疗視视覺觉觀观眾众連连續续總总結结練练記记憶忆歸归層层狀状態态環环境境舊旧種种曆历麵面餘余滿满緩缓潔洁鬆松傢家剛刚劃划麗丽雙双燒烧輕轻鬧闹補补衝冲陸陆陳陈隴陇陝陕陣阵責责任任擁拥擠挤擬拟揮挥搖摇損损掃扫掛挂採采挑挑揀拣棄弃擔担攝摄撥拨擴扩擾扰響响幫帮帶带師师幣币並并論论討讨認认識识証证證证訊讯許许譯译詩诗誰谁謝谢謂谓諸诸變变贏赢輸输貴贵賞赏贊赞趨趋趕赶躍跃辦办農农適适遞递進进遊游遙遥鄉乡鄰邻釋释鎖锁銀银銅铜鐵铁鐘钟鍵键錯错鏡镜隊队陽阳陰阴隱隐雖虽需需靜静韓韩領领頻频顏颜願愿類类顧顾飯饭館馆飲饮餅饼餓饿餵喂騎骑驚惊驗验鮮鲜魚鱼鳥鸟雞鸡鹽盐麥麦齊齐齒齿龍龙龜龟";
    let mut map = HashMap::new();
    let mut pairs = PAIRS.chars();
    while let (Some(from), Some(to)) = (pairs.next(), pairs.next()) { map.insert(from, to); }
    text.chars().map(|c| map.get(&c).copied().unwrap_or(c)).collect()
}

fn integer(s: &str) -> String {
    const DIGITS: [char; 10] = ['零','一','二','三','四','五','六','七','八','九'];
    if s.len() > 16 || s.starts_with('0') && s.len() > 1 {
        return s.chars().filter_map(|c| c.to_digit(10).map(|n| DIGITS[n as usize])).collect();
    }
    let Ok(mut n) = s.parse::<u64>() else { return s.into(); };
    if n == 0 { return "零".into(); }
    fn group(n: u64) -> String {
        let mut out = String::new();
        let mut zero = false;
        for (div, unit) in [(1000,"千"),(100,"百"),(10,"十"),(1,"")] {
            let d = (n / div) % 10;
            if d > 0 {
                if zero { out.push('零'); zero = false; }
                out.push(DIGITS[d as usize]); out.push_str(unit);
            } else if !out.is_empty() && n % div > 0 { zero = true; }
        }
        out
    }
    let mut groups = Vec::new();
    while n > 0 { groups.push(n % 10000); n /= 10000; }
    let mut out = String::new();
    let mut gap = false;
    for i in (0..groups.len()).rev() {
        let n = groups[i];
        if n == 0 { gap = true; continue; }
        if !out.is_empty() && (gap || n < 1000) { out.push('零'); }
        out.push_str(&group(n)); out.push_str(["","万","亿","万亿"][i]); gap = false;
    }
    if out.starts_with("一十") { out.remove(0); }
    out
}

fn numbers(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_ascii_digit() || c[i] == '-' && c.get(i+1).is_some_and(char::is_ascii_digit) {
            let negative = c[i] == '-';
            if negative { i += 1; }
            let start = i;
            while i < c.len() && c[i].is_ascii_digit() { i += 1; }
            let mut value = integer(&c[start..i].iter().collect::<String>());
            if c.get(i) == Some(&'.') && c.get(i+1).is_some_and(char::is_ascii_digit) {
                value.push('点'); i += 1;
                while i < c.len() && c[i].is_ascii_digit() {
                    value.push(['零','一','二','三','四','五','六','七','八','九'][c[i] as usize - '0' as usize]); i += 1;
                }
            }
            if c.get(i).is_some_and(|c| matches!(*c, '%'|'％')) { out.push_str("百分之"); i += 1; }
            if negative { out.push('负'); }
            out.push_str(&value);
        } else { out.push(c[i]); i += 1; }
    }
    out
}

fn punctuation(c: char) -> char {
    match c {
        '、'|'，' => ',', '。'|'．' => '.', '！' => '!', '：' => ':',
        '；' => ';', '？' => '?', '«'|'《'|'「'|'【' => '“',
        '»'|'》'|'」'|'】' => '”', '（' => '(', '）' => ')', _ => c,
    }
}

fn pinyin(word: &str) -> Vec<(String,u8)> {
    // Source overrides that differ from the single-character dictionary.
    let phrase = match word {
        "开户行" => Some("kai1 hu4 hang2"), "发卡行" => Some("fa4 ka3 hang2"),
        "放款行" => Some("fang4 kuan3 hang2"), "茧行" => Some("jian3 hang2"),
        "行号" => Some("hang2 hao4"), "各地" => Some("ge4 di4"),
        "借还款" => Some("jie4 huan2 kuan3"), "时间为" => Some("shi2 jian1 wei2"),
        "为准" => Some("wei2 zhun3"), "色差" => Some("se4 cha1"),
        "掺和" => Some("chan1 huo5"), "嗲" => Some("dia3"), "呗" => Some("bei5"),
        "咗" => Some("zuo5"), "嘞" => Some("lei5"), _ => None,
    };
    fn parse(py: &str) -> (String,u8) {
        let last = py.as_bytes().last().copied().unwrap_or(b'5');
        if (b'1'..=b'5').contains(&last) { (py[..py.len()-1].replace('ü', "v"), last-b'0') }
        else { (py.replace('ü', "v"), 5) }
    }
    if let Some(p) = phrase { return p.split_whitespace().map(parse).collect(); }
    word.chars().map(|c| {
        if c == '地' { return ("de".into(), 5); }
        if c == '嗯' { return ("n".into(), 2); }
        c.to_pinyin().map(|p| parse(p.with_tone_num_end())).unwrap_or_default()
    }).collect()
}

/// Convert orthographic pinyin into misaki's initials/finals symbols.
fn syllable(py: &str, tone: u8, erhua: bool) -> String {
    if py.is_empty() { return String::new(); }
    let py = match py {
        "yi" => "i".into(), "yin" => "in".into(), "ying" => "ing".into(),
        "wu" => "u".into(), "yu" => "v".into(), "yue" => "ve".into(),
        "yuan" => "van".into(), "yun" => "vn".into(),
        _ if py.starts_with('y') => format!("i{}", &py[1..]),
        _ if py.starts_with('w') => format!("u{}", &py[1..]), _ => py.to_owned(),
    };
    let initial = ["zh","ch","sh","b","p","m","f","d","t","n","l","g","k","h","j","q","x","r","z","c","s"]
        .into_iter().find(|i| py.starts_with(*i)).unwrap_or("");
    let final_ = &py[initial.len()..];
    let final_ = match (initial,final_) {
        ("z"|"c"|"s","i") => "ii", ("zh"|"ch"|"sh"|"r","i") => "iii",
        ("j"|"q"|"x","u") => "v", ("j"|"q"|"x","ue") => "ve",
        ("j"|"q"|"x","uan") => "van", ("j"|"q"|"x","un") => "vn",
        (_,"iu") => "iou", (_,"ui") => "uei", (_,"un") => "uen", (_,"ue") => "ve", (_,f) => f,
    };
    let lookup = |s| ZH_MAP.iter().find(|p| p.0 == s).map(|p| p.1).unwrap_or("");
    let mut out = format!("{}{}", lookup(initial), lookup(if erhua && final_ == "e" { "er" } else { final_ }));
    if erhua && final_ != "e" { out.push('R'); }
    if (1..=5).contains(&tone) { out.push(char::from(b'0'+tone)); }
    out
}

fn chinese(text: &str) -> String {
    let jieba = JIEBA.get_or_init(Jieba::new);
    let words = jieba.tag(text, false).into_iter().map(|t| Word {
        text: t.word.into(), pos: if t.tag == "x" && t.word.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) { "X".into() } else { t.tag.into() },
        syllables: pinyin(t.word),
    }).collect();
    let mut out = String::new();
    let mut prev_chinese = false;
    for mut word in tone_sandhi::pre_merge(words) {
        if word.pos == "x" || word.pos == "eng" {
            out.extend(word.text.chars().map(punctuation)); prev_chinese = false; continue;
        }
        if prev_chinese { out.push('/'); }
        tone_sandhi::modify(&mut word, jieba);
        let len = word.syllables.len();
        let merge_er = len > 1 && word.text.ends_with('儿')
            && matches!(word.syllables[len-1].0.as_str(), "er")
            && (MUST_ERHUA.contains(&word.text.as_str()) || !NOT_ERHUA.contains(&word.text.as_str()) && !matches!(word.pos.as_str(), "a"|"j"|"nr"))
            && !NOT_ERHUA.iter().any(|w| word.text.ends_with(*w));
        for (i,(py,tone)) in word.syllables.iter().enumerate() {
            if merge_er && i == len-1 { break; }
            out.push_str(&syllable(py, *tone, merge_er && i == len-2));
        }
        prev_chinese = true;
    }
    out
}

pub fn is_pure_english(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_alphabetic())
        && !text.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c))
}

fn phonemes_with(text: &str, english: impl Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let text = simplify(&numbers(text));
    let chars: Vec<_> = text.chars().collect();
    let mut i = 0;
    let mut parts = Vec::new();
    while i < chars.len() {
        let en = chars[i].is_ascii_alphabetic();
        let start = i; i += 1;
        while i < chars.len() && if en {
            chars[i].is_ascii_alphabetic() || matches!(chars[i], ' '| '\'' | '-')
        } else { !chars[i].is_ascii_alphabetic() } { i += 1; }
        let segment: String = chars[start..i].iter().collect();
        if en { parts.push(english(segment.trim())?); }
        else { parts.push(chinese(&segment)); }
    }
    Ok(parts.join(" ").chars().filter(|c| vocabulary().contains_key(c)).collect())
}

fn phonemes(text: &str, model_dir: &Path) -> Result<String,String> {
    phonemes_with(text, |s| english::phonemes(s, model_dir))
}

/// Sentence boundaries are preferred; the hard limit also handles a single
/// unbroken sentence. Padding is added by the caller, never counted here.
pub fn chunks(text: &str, model_dir: &Path) -> Result<Vec<Vec<i64>>, String> {
    let phones = phonemes(text, model_dir)?;
    Ok(token_chunks(&phones))
}

fn token_chunks(phones: &str) -> Vec<Vec<i64>> {
    let vocab = vocabulary();
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    for c in phones.chars() {
        let Some(id) = vocab.get(&c) else { continue; };
        if *id == 0 { continue; }
        chunk.push(*id);
        if chunk.len() == 500 || matches!(c, '.'|'!'|'?'|';') {
            chunks.push(std::mem::take(&mut chunk));
        }
    }
    if !chunk.is_empty() { chunks.push(chunk); }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn tone_and_symbol_table() {
        assert_eq!(syllable("zhi", 1, false), "ㄓ十1");
        assert_eq!(syllable("yuan", 2, false), "元2");
        assert_eq!(syllable("jue", 3, false), "ㄐ月3");
        assert_eq!(syllable("shui", 4, false), "ㄕ为4");
        assert_eq!(syllable("liu", 2, false), "ㄌ又2");
        assert_eq!(syllable("zi", 5, false), "ㄗㄭ5");
        assert_eq!(syllable("ge", 1, true), "ㄍㄦ1");
        assert_eq!(chinese("你好"), "ㄋㄧ2ㄏㄠ3");
    }
    #[test] fn traditional_numbers_and_punctuation() {
        assert_eq!(simplify("臺灣"), "台湾");
        assert_eq!(chinese(&simplify("臺灣")), chinese("台湾"));
        assert_eq!(numbers("3.5 85%"), "三点五 百分之八十五");
        assert_eq!(numbers("101 10001 10"), "一百零一 一万零一 十");
        assert_eq!("，。！？「」".chars().map(punctuation).collect::<String>(), ",.!?“”");
    }
    #[test] fn erhua_and_unknown_cjk_are_safe() {
        assert!(chinese("小院儿").contains('R'));
        assert!(!chinese("女儿").contains('R'));
        let phones = phonemes("你好👩‍💻𠀀！", Path::new("/unused")).unwrap();
        let ids = chunks("你好👩‍💻𠀀！", Path::new("/unused")).unwrap();
        assert!(!ids.is_empty());
        assert!(phones.chars().all(|c| vocabulary().contains_key(&c)));
        assert!(!phones.contains('👩'));
    }
    #[test] fn empty_and_long_chunks() {
        assert!(chunks("", Path::new("/unused")).unwrap().is_empty());
        let result = chunks(&"你好".repeat(2000), Path::new("/unused")).unwrap();
        assert!(result.len() > 1);
        assert!(result.iter().all(|c| !c.is_empty() && c.len() <= 500));
    }
    #[test] fn mixed_english_uses_lexicon_and_vocab() {
        let lexicon = HashMap::from([("odoo".into(), "oʊdu".into())]);
        let phones = phonemes_with("Odoo 設定", |s| Ok(english::lookup(s, &lexicon))).unwrap();
        assert!(phones.starts_with("oʊdu"));
        assert!(phones.contains("ㄕㄜ4"));
        assert!(phones.chars().all(|c| vocabulary().contains_key(&c)));
        let tokens = token_chunks(&phones);
        assert!(!tokens.is_empty());
        assert!(tokens.iter().flatten().all(|id| vocabulary().values().any(|v| v == id)));
    }
    #[test] fn english_detection() {
        assert!(is_pure_english("Hello world!"));
        assert!(!is_pure_english("Odoo 設定"));
    }
}
