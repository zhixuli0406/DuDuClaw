//! Chinese script-variant helpers (Simplified ↔ Traditional).
//!
//! DuDuClaw's primary audience is zh-TW. Utility-LLM outputs that are *not*
//! part of a user-authored conversation — auto-generated session titles,
//! summaries, labels — must not come back in Simplified characters just
//! because the utility model happened to default to mainland conventions
//! (WP11-C, 2026-08-04: a zh-TW customer's conversation list showed
//! 「了解用户个人档案信息」).
//!
//! Scope and honesty about limits:
//! - This is **not** a general S→T converter (no OpenCC-grade phrase table).
//!   It is a curated, deterministic, one-to-one map over ~300 Simplified-only
//!   characters whose Traditional counterpart is unambiguous in ordinary prose.
//! - Characters with genuinely ambiguous mappings (复 → 復/複/覆, 冲 → 沖/衝,
//!   余 → 余/餘 …) are **detected but never rewritten** — see
//!   [`AMBIGUOUS_SIMPLIFIED`]. Callers should treat detection as a signal to
//!   fix the *prompt*, not to force a guess.
//! - Characters that are valid in BOTH scripts (后, 里, 只, 面, 干, 云, 台 …)
//!   are deliberately absent — rewriting them would corrupt Traditional text.
//!
//! The primary defence is always the prompt (ask the model for zh-TW);
//! these helpers are the deterministic safety net behind it.

/// Curated Simplified → Traditional character map, sorted by the Simplified
/// character so lookups can binary-search. Only unambiguous one-to-one
/// mappings live here.
const SIMP_TO_TRAD: &[(char, char)] = &[
    ('与', '與'), ('业', '業'), ('东', '東'), ('个', '個'), ('为', '為'), ('丽', '麗'), ('么', '麼'), ('义', '義'),
    ('习', '習'), ('乡', '鄉'), ('书', '書'), ('买', '買'), ('争', '爭'), ('亏', '虧'), ('产', '產'), ('亲', '親'),
    ('们', '們'), ('价', '價'), ('优', '優'), ('会', '會'), ('传', '傳'), ('体', '體'), ('侦', '偵'), ('债', '債'),
    ('储', '儲'), ('儿', '兒'), ('兑', '兌'), ('关', '關'), ('养', '養'), ('册', '冊'), ('农', '農'), ('决', '決'),
    ('况', '況'), ('冻', '凍'), ('净', '淨'), ('凉', '涼'), ('减', '減'), ('凤', '鳳'), ('击', '擊'), ('则', '則'),
    ('删', '刪'), ('剂', '劑'), ('务', '務'), ('动', '動'), ('励', '勵'), ('势', '勢'), ('区', '區'), ('医', '醫'),
    ('华', '華'), ('单', '單'), ('卖', '賣'), ('卫', '衛'), ('厂', '廠'), ('厅', '廳'), ('压', '壓'), ('县', '縣'),
    ('双', '雙'), ('变', '變'), ('号', '號'), ('启', '啟'), ('员', '員'), ('响', '響'), ('团', '團'), ('园', '園'),
    ('围', '圍'), ('国', '國'), ('图', '圖'), ('场', '場'), ('块', '塊'), ('声', '聲'), ('处', '處'), ('备', '備'),
    ('够', '夠'), ('头', '頭'), ('夹', '夾'), ('奖', '獎'), ('妇', '婦'), ('孙', '孫'), ('学', '學'), ('实', '實'),
    ('审', '審'), ('宾', '賓'), ('导', '導'), ('层', '層'), ('属', '屬'), ('岁', '歲'), ('币', '幣'), ('库', '庫'),
    ('应', '應'), ('废', '廢'), ('开', '開'), ('异', '異'), ('弃', '棄'), ('张', '張'), ('弹', '彈'), ('归', '歸'),
    ('录', '錄'), ('忆', '憶'), ('态', '態'), ('总', '總'), ('户', '戶'), ('执', '執'), ('扩', '擴'), ('护', '護'),
    ('报', '報'), ('择', '擇'), ('挡', '擋'), ('损', '損'), ('换', '換'), ('数', '數'), ('断', '斷'), ('旧', '舊'),
    ('时', '時'), ('显', '顯'), ('机', '機'), ('权', '權'), ('条', '條'), ('构', '構'), ('标', '標'), ('栈', '棧'),
    ('样', '樣'), ('档', '檔'), ('检', '檢'), ('楼', '樓'), ('汉', '漢'), ('洁', '潔'), ('测', '測'), ('浏', '瀏'),
    ('润', '潤'), ('温', '溫'), ('湿', '濕'), ('满', '滿'), ('灭', '滅'), ('灯', '燈'), ('灵', '靈'), ('点', '點'),
    ('烟', '煙'), ('热', '熱'), ('爱', '愛'), ('状', '狀'), ('狱', '獄'), ('猪', '豬'), ('献', '獻'), ('现', '現'),
    ('电', '電'), ('疗', '療'), ('监', '監'), ('盖', '蓋'), ('盘', '盤'), ('矿', '礦'), ('码', '碼'), ('确', '確'),
    ('稳', '穩'), ('竞', '競'), ('类', '類'), ('纠', '糾'), ('约', '約'), ('级', '級'), ('纪', '紀'), ('纷', '紛'),
    ('线', '線'), ('组', '組'), ('织', '織'), ('终', '終'), ('结', '結'), ('络', '絡'), ('继', '繼'), ('绩', '績'),
    ('续', '續'), ('维', '維'), ('缆', '纜'), ('缉', '緝'), ('缓', '緩'), ('编', '編'), ('缩', '縮'), ('网', '網'),
    ('罚', '罰'), ('职', '職'), ('脑', '腦'), ('脸', '臉'), ('苏', '蘇'), ('荣', '榮'), ('药', '藥'), ('营', '營'),
    ('虑', '慮'), ('补', '補'), ('装', '裝'), ('规', '規'), ('视', '視'), ('览', '覽'), ('触', '觸'), ('誉', '譽'),
    ('计', '計'), ('认', '認'), ('讨', '討'), ('让', '讓'), ('议', '議'), ('讯', '訊'), ('记', '記'), ('许', '許'),
    ('论', '論'), ('讼', '訟'), ('设', '設'), ('访', '訪'), ('证', '證'), ('评', '評'), ('识', '識'), ('诉', '訴'),
    ('译', '譯'), ('试', '試'), ('询', '詢'), ('该', '該'), ('语', '語'), ('误', '誤'), ('说', '說'), ('请', '請'),
    ('谁', '誰'), ('调', '調'), ('谈', '談'), ('负', '負'), ('财', '財'), ('责', '責'), ('账', '帳'), ('质', '質'),
    ('购', '購'), ('贴', '貼'), ('贷', '貸'), ('贸', '貿'), ('费', '費'), ('资', '資'), ('赠', '贈'), ('赢', '贏'),
    ('车', '車'), ('转', '轉'), ('软', '軟'), ('载', '載'), ('辆', '輛'), ('辑', '輯'), ('输', '輸'), ('辩', '辯'),
    ('边', '邊'), ('达', '達'), ('迁', '遷'), ('过', '過'), ('这', '這'), ('进', '進'), ('违', '違'), ('连', '連'),
    ('选', '選'), ('递', '遞'), ('遗', '遺'), ('邮', '郵'), ('释', '釋'), ('钢', '鋼'), ('钥', '鑰'), ('钱', '錢'),
    ('铁', '鐵'), ('铜', '銅'), ('铝', '鋁'), ('银', '銀'), ('销', '銷'), ('锁', '鎖'), ('错', '錯'), ('键', '鍵'),
    ('镇', '鎮'), ('长', '長'), ('门', '門'), ('闭', '閉'), ('问', '問'), ('阅', '閱'), ('队', '隊'), ('阳', '陽'),
    ('阴', '陰'), ('际', '際'), ('险', '險'), ('隐', '隱'), ('静', '靜'), ('页', '頁'), ('项', '項'), ('领', '領'),
    ('频', '頻'), ('题', '題'), ('颜', '顏'), ('饮', '飲'), ('馆', '館'), ('马', '馬'), ('驱', '驅'), ('验', '驗'),
    ('鱼', '魚'), ('鲜', '鮮'), ('鸟', '鳥'), ('鸡', '雞'), ('鸭', '鴨'), ('齿', '齒'), ('龄', '齡'), ('龙', '龍'),
];

/// Simplified-only characters whose Traditional counterpart depends on the
/// word, so we detect them but never rewrite them (sorted).
///
/// 冲 沖/衝 · 划 划/劃 · 别 別/彆 · 历 歷/曆 · 发 發/髮 · 复 復/複/覆 ·
/// 尽 盡/儘 · 汇 匯/彙 · 签 簽/籤 · 钟 鐘/鍾
///
/// Characters valid in BOTH scripts (游, 丰, 术, 据, 种, 后, 里, 只, 面, 干,
/// 云, 台, 表, 谷, 松, 板, 系, 制 …) appear in NEITHER table — they are
/// ordinary Traditional characters and must not even be *flagged*, or
/// [`dominant_variant`] would misread 「上游 API」 as Simplified.
const AMBIGUOUS_SIMPLIFIED: &[char] = &['冲', '划', '别', '历', '发', '复', '尽', '汇', '签', '钟'];

/// Which Chinese script a piece of text is written in, for the purpose of
/// "answer in the same language the user used".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChineseVariant {
    /// No (or negligible) Han characters — not a Chinese text.
    None,
    /// Han text with Simplified-only characters present.
    Simplified,
    /// Han text with no Simplified-only characters — treated as Traditional.
    Traditional,
}

/// True for CJK Unified Ideographs (BMP block + Extension A).
fn is_han(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
}

fn trad_for(c: char) -> Option<char> {
    SIMP_TO_TRAD
        .binary_search_by_key(&c, |&(s, _)| s)
        .ok()
        .map(|i| SIMP_TO_TRAD[i].1)
}

/// True when `s` contains at least one character that only exists in the
/// Simplified script (either convertible or [`AMBIGUOUS_SIMPLIFIED`]).
///
/// Never true for pure Traditional text — characters valid in both scripts
/// are excluded from both tables by construction.
pub fn contains_simplified(s: &str) -> bool {
    s.chars()
        .any(|c| trad_for(c).is_some() || AMBIGUOUS_SIMPLIFIED.binary_search(&c).is_ok())
}

/// Rewrite the Simplified characters in `s` that have an unambiguous
/// Traditional counterpart. Everything else (including
/// [`AMBIGUOUS_SIMPLIFIED`], punctuation, Latin, emoji) passes through
/// byte-for-byte.
///
/// Deterministic, allocation-light, and a no-op on text that contains no
/// mapped character.
pub fn to_traditional(s: &str) -> String {
    if !s.chars().any(|c| trad_for(c).is_some()) {
        return s.to_string();
    }
    s.chars().map(|c| trad_for(c).unwrap_or(c)).collect()
}

/// Classify the dominant Chinese script of `s`.
///
/// Requires at least `MIN_HAN` Han characters before committing to a verdict
/// so a stray CJK glyph inside an English sentence cannot flip the result.
pub fn dominant_variant(s: &str) -> ChineseVariant {
    const MIN_HAN: usize = 4;
    let han = s.chars().filter(|&c| is_han(c)).count();
    if han < MIN_HAN {
        return ChineseVariant::None;
    }
    if contains_simplified(s) {
        ChineseVariant::Simplified
    } else {
        ChineseVariant::Traditional
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_free_of_identity_pairs() {
        for w in SIMP_TO_TRAD.windows(2) {
            assert!(w[0].0 < w[1].0, "table must be sorted & unique: {:?}", w);
        }
        for &(s, t) in SIMP_TO_TRAD {
            assert_ne!(s, t, "identity mapping is pointless: {s}");
        }
        for w in AMBIGUOUS_SIMPLIFIED.windows(2) {
            assert!(w[0] < w[1], "ambiguous list must be sorted: {:?}", w);
        }
        // The two tables must not overlap — a char is either rewritable or not.
        for &c in AMBIGUOUS_SIMPLIFIED {
            assert!(trad_for(c).is_none(), "{c} is in both tables");
        }
    }

    #[test]
    fn converts_the_reported_simplified_title() {
        // WP11-C field evidence: the zh-TW customer's session list showed this.
        assert_eq!(to_traditional("了解用户个人档案信息"), "了解用戶個人檔案信息");
        assert!(!contains_simplified("了解用戶個人檔案信息"));
    }

    #[test]
    fn detects_the_common_variant_pairs() {
        for (simp, trad) in [("用户", "用戶"), ("档案", "檔案"), ("意义", "意義")] {
            assert!(contains_simplified(simp), "{simp} must be flagged");
            assert!(!contains_simplified(trad), "{trad} must NOT be flagged");
            assert_eq!(to_traditional(simp), trad);
        }
    }

    #[test]
    fn traditional_text_passes_through_untouched() {
        let cases = [
            "週報排程討論",
            "Deploy pipeline 部署流程",
            "後面只有里長在干活", // chars valid in BOTH scripts must survive
            "台北雲端表格系統",
            // Review counter-examples: these five were wrongly listed as
            // Simplified-only in the first cut and would have been corrupted.
            "游泳池的水溫偏低",   // 游
            "丰采依舊的老先生",   // 丰
            "手頭拮据只好省著用", // 据
            "白术是一味中藥材",   // 术
            "种姓制度的歷史脈絡", // 种
        ];
        for c in cases {
            assert_eq!(to_traditional(c), c, "must not rewrite: {c}");
            assert!(!contains_simplified(c), "must not flag: {c}");
        }
    }

    /// Characters valid in both scripts must not drag a Traditional text into
    /// the Simplified bucket — otherwise the titler would stop asking for
    /// zh-TW exactly when it matters.
    #[test]
    fn shared_chars_do_not_flip_the_variant_verdict() {
        for text in [
            "上游 API 對話紀錄整理",
            "游標移動與水溫監控",
            "丰采、拮据、白术、种姓這些詞都是正體",
        ] {
            assert_eq!(
                dominant_variant(text),
                ChineseVariant::Traditional,
                "must stay Traditional: {text}"
            );
        }
    }

    /// Multi-mapping characters are detected but never guessed at.
    #[test]
    fn multi_mapping_chars_are_detect_only() {
        // 发 → 發 (开发) or 髮 (头发) — context decides, so we refuse to pick.
        assert!(contains_simplified("头发"));
        assert_eq!(to_traditional("头发"), "頭发", "头 converts, 发 must not");
        for (text, ch) in [
            ("重复检查", '复'),
            ("日历表", '历'),
            ("尽量处理", '尽'),
            ("汇总报告", '汇'),
            ("签名档", '签'),
            ("闹钟设定", '钟'),
            ("别的问题", '别'),
            ("冲突处理", '冲'),
            ("划分区块", '划'),
        ] {
            assert!(contains_simplified(text), "{text} must be flagged");
            assert!(
                to_traditional(text).contains(ch),
                "{ch} must survive untouched in {text}"
            );
        }
    }

    /// zh-TW writes 帳 (帳號/帳戶), not the mainland 賬.
    #[test]
    fn account_char_uses_the_taiwan_form() {
        assert_eq!(to_traditional("账户资料"), "帳戶資料");
    }

    #[test]
    fn ambiguous_chars_are_detected_but_not_rewritten() {
        assert!(contains_simplified("重复检查"));
        // 复 stays; 检 is rewritten.
        assert_eq!(to_traditional("重复检查"), "重复檢查");
    }

    #[test]
    fn dominant_variant_classifies() {
        assert_eq!(dominant_variant("我們今天討論排程問題"), ChineseVariant::Traditional);
        assert_eq!(dominant_variant("我们今天讨论排程问题"), ChineseVariant::Simplified);
        assert_eq!(dominant_variant("deploy the pipeline now"), ChineseVariant::None);
        // Too few Han characters to judge.
        assert_eq!(dominant_variant("ok 好"), ChineseVariant::None);
    }

    #[test]
    fn non_han_input_is_untouched() {
        let s = "Hello, world! 🐾 <b>ok</b>";
        assert_eq!(to_traditional(s), s);
        assert!(!contains_simplified(s));
    }
}
