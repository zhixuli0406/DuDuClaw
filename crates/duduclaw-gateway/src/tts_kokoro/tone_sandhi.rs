// Ported from hexgrad/misaki tone_sandhi.py, adapted from PaddleSpeech.
// Copyright (c) 2021 PaddlePaddle Authors. All Rights Reserved.
// Copyright hexgrad (misaki contributors).
// Licensed under Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0
// Modified: Rust implementation using jieba-rs and explicit syllable tones.

use jieba_rs::Jieba;

const NEUTRAL: &[&str] = &["麻烦","麻利","鸳鸯","高粱","骨头","骆驼","马虎","首饰","馒头","馄饨","风筝","难为","队伍","阔气","闺女","门道","锄头","铺盖","铃铛","铁匠","钥匙","里脊","里头","部分","那么","道士","造化","迷糊","连累","这么","这个","运气","过去","软和","转悠","踏实","跳蚤","跟头","趔趄","财主","豆腐","讲究","记性","记号","认识","规矩","见识","裁缝","补丁","衣裳","衣服","衙门","街坊","行李","行当","蛤蟆","蘑菇","薄荷","葫芦","葡萄","萝卜","荸荠","苗条","苗头","苍蝇","芝麻","舒服","舒坦","舌头","自在","膏药","脾气","脑袋","脊梁","能耐","胳膊","胭脂","胡萝","胡琴","胡同","聪明","耽误","耽搁","耷拉","耳朵","老爷","老实","老婆","戏弄","将军","翻腾","罗嗦","罐头","编辑","结实","红火","累赘","糨糊","糊涂","精神","粮食","簸箕","篱笆","算计","算盘","答应","笤帚","笑语","笑话","窟窿","窝囊","窗户","稳当","稀罕","称呼","秧歌","秀气","秀才","福气","祖宗","砚台","码头","石榴","石头","石匠","知识","眼睛","眯缝","眨巴","眉毛","相声","盘算","白净","痢疾","痛快","疟疾","疙瘩","疏忽","畜生","生意","甘蔗","琵琶","琢磨","琉璃","玻璃","玫瑰","玄乎","狐狸","状元","特务","牲口","牙碜","牌楼","爽快","爱人","热闹","烧饼","烟筒","烂糊","点心","炊帚","灯笼","火候","漂亮","滑溜","溜达","温和","清楚","消息","浪头","活泼","比方","正经","欺负","模糊","槟榔","棺材","棒槌","棉花","核桃","栅栏","柴火","架势","枕头","枇杷","机灵","本事","木头","木匠","朋友","月饼","月亮","暖和","明白","时候","新鲜","故事","收拾","收成","提防","挖苦","挑剔","指甲","指头","拾掇","拳头","拨弄","招牌","招呼","抬举","护士","折腾","扫帚","打量","打算","打扮","打听","打发","扎实","扁担","戒指","懒得","意识","意思","悟性","怪物","思量","怎么","念头","念叨","别人","快活","忙活","志气","心思","得罪","张罗","弟兄","开通","应酬","庄稼","干事","帮手","帐篷","希罕","师父","师傅","巴结","巴掌","差事","工夫","岁数","屁股","尾巴","少爷","小气","小伙","将就","对头","对付","寡妇","家伙","客气","实在","官司","学问","字号","嫁妆","媳妇","媒人","婆家","娘家","委屈","姑娘","姐夫","妯娌","妥当","妖精","奴才","女婿","头发","太阳","大爷","大方","大意","大夫","多少","多么","外甥","壮实","地道","地方","在乎","困难","嘴巴","嘱咐","嘟囔","嘀咕","喜欢","喇嘛","喇叭","商量","唾沫","哑巴","哈欠","哆嗦","咳嗽","和尚","告诉","告示","含糊","吓唬","后头","名字","名堂","合同","吆喝","叫唤","口袋","厚道","厉害","千斤","包袱","包涵","匀称","勤快","动静","动弹","功夫","力气","前头","刺猬","刺激","别扭","利落","利索","利害","分析","出息","凑合","凉快","冷战","冤枉","冒失","养活","关系","先生","兄弟","便宜","使唤","佩服","作坊","体面","位置","似的","伙计","休息","什么","人家","亲戚","亲家","交情","云彩","事情","买卖","主意","丫头","丧气","两口","东西","东家","世故","不由","下水","下巴","上头","上司","丈夫","丈人","一辈","那个","菩萨","父亲","母亲","咕噜","邋遢","费用","冤家","甜头","介绍","荒唐","大人","泥鳅","幸福","熟悉","计划","扑腾","蜡烛","姥爷","照顾","喉咙","吉他","弄堂","蚂蚱","凤凰","拖沓","寒碜","糟蹋","倒腾","报复","逻辑","盘缠","喽啰","牢骚","咖喱","扫把","惦记"];
const NOT_NEUTRAL: &[&str] = &["男子","女子","分子","原子","量子","莲子","石子","瓜子","电子","人人","虎虎","幺幺","干嘛","学子","哈哈","数数","袅袅","局地","以下","娃哈哈","花花草草","留得","耕地","想想","熙熙","攘攘","卵子","死死","冉冉","恳恳","佼佼","吵吵","打打","考考","整整","莘莘","落地","算子","家家户户","青青"];

#[derive(Clone, Debug)]
pub(super) struct Word {
    pub text: String,
    pub pos: String,
    pub syllables: Vec<(String, u8)>,
}
impl Word {
    fn merge(&mut self, other: Word) {
        self.text.push_str(&other.text);
        self.syllables.extend(other.syllables);
    }
    fn speakable(&self) -> bool { !matches!(self.pos.as_str(), "x" | "eng") }
    fn repeated(&self) -> bool {
        let c: Vec<_> = self.text.chars().collect();
        c.len() == 2 && c[0] == c[1]
    }
}

/// The source pre-merge order matters: 不, 一, reduplication, thirds, 儿.
pub(super) fn pre_merge(words: Vec<Word>) -> Vec<Word> {
    let mut words = words;
    for pass in 0..6 {
        let mut out: Vec<Word> = Vec::new();
        let mut it = words.into_iter().peekable();
        let mut merged_last = false;
        while let Some(word) = it.next() {
            if pass == 1 && word.text == "一" && word.speakable() {
                if let (Some(prev), Some(next)) = (out.last_mut(), it.peek()) {
                    if prev.text == next.text && prev.pos == "v" && next.speakable() {
                        prev.merge(word);
                        prev.merge(it.next().unwrap());
                        continue;
                    }
                }
            }
            let merge = out.last().is_some_and(|prev| {
                if !word.speakable() || !prev.speakable() { return false; }
                match pass {
                    0 => prev.text == "不",
                    1 => prev.text == "一",
                    2 => prev.text == word.text,
                    3 | 4 => {
                        let tones = |w: &Word| w.syllables.iter().all(|s| s.1 == 3);
                        let adjacent = prev.syllables.last().is_some_and(|s| s.1 == 3)
                            && word.syllables.first().is_some_and(|s| s.1 == 3);
                        !merged_last && !prev.repeated() && prev.syllables.len() + word.syllables.len() <= 3
                            && if pass == 3 { tones(prev) && tones(&word) } else { adjacent }
                    }
                    5 => word.text == "儿",
                    _ => false,
                }
            });
            if merge { out.last_mut().unwrap().merge(word); } else { out.push(word); }
            merged_last = merge;
        }
        words = out;
    }
    words
}

fn split(word: &str, jieba: &Jieba) -> usize {
    let mut subs = jieba.cut_for_search(word, false);
    subs.sort_by_key(|s| s.word.chars().count());
    let n = word.chars().count();
    let Some(first) = subs.first() else { return n; };
    let len = first.word.chars().count();
    if word.starts_with(first.word) { len } else { n.saturating_sub(len) }
}
fn conventional(word: &str) -> bool {
    NEUTRAL.contains(&word) || {
        let tail: String = word.chars().rev().take(2).collect::<Vec<_>>().into_iter().rev().collect();
        NEUTRAL.contains(&tail.as_str())
    }
}

fn is_numeral(c: char) -> bool {
    // Python str.isnumeric() includes Han numerals, unlike Rust char::is_numeric().
    c.is_numeric() || "零一二三四五六七八九十百千万萬亿億兆壹贰貳叁參肆伍陆陸柒捌玖拾佰仟廿卅卌两兩".contains(c)
}

pub(super) fn modify(word: &mut Word, jieba: &Jieba) {
    let chars: Vec<char> = word.text.chars().collect();
    if chars.len() != word.syllables.len() || chars.is_empty() { return; }
    let n = chars.len();
    let s = &mut word.syllables;
    // 不: neutral in A不B; second tone before a fourth tone.
    for i in 0..n {
        if chars[i] == '不' {
            if n == 3 && i == 1 { s[i].1 = 5; }
            else if i + 1 < n && s[i + 1].1 == 4 { s[i].1 = 2; }
        }
    }
    let number = chars.contains(&'一')
        && chars.iter().all(|c| is_numeral(*c));
    if !number {
        if n == 3 && chars[1] == '一' && chars[0] == chars[2] { s[1].1 = 5; }
        else if word.text.starts_with("第一") { s[1].1 = 1; }
        else {
            for i in 0..n.saturating_sub(1) {
                if chars[i] == '一' && !"、：，；。？！“”‘’':,;.?!".contains(chars[i+1]) {
                    s[i].1 = if matches!(s[i+1].1, 4 | 5) { 2 } else { 4 };
                }
            }
        }
    }
    // Numeric repetitions are digits, not noun/verb reduplication, even when
    // a minimal dictionary tags the whole sequence as a noun.
    if !number && !NOT_NEUTRAL.contains(&word.text.as_str()) {
        for i in 1..n {
            if chars[i] == chars[i-1] && word.pos.chars().next().is_some_and(|c| matches!(c, 'n'|'v'|'a')) { s[i].1 = 5; }
        }
        let last = chars[n-1];
        let neutral = "吧呢啊呐噻嘛吖嗨哦哒滴哩哟喽啰耶喔诶的地得".contains(last)
            || (n == 1 && "了着过".contains(last) && matches!(word.pos.as_str(), "ul"|"uz"|"ug"))
            || (n > 1 && "们子".contains(last) && matches!(word.pos.as_str(), "r"|"n"))
            || (n > 1 && "上下".contains(last) && matches!(word.pos.as_str(), "s"|"l"|"f"))
            || (n > 1 && "来去".contains(last) && "上下进出回过起开".contains(chars[n-2]));
        if neutral { s[n-1].1 = 5; }
        else if let Some(i) = chars.iter().position(|c| *c == '个').filter(|i| *i == 0 && n == 1 || *i > 0 && (is_numeral(chars[*i-1]) || "几有两半多各整每做是".contains(chars[*i-1]))) {
            s[i].1 = 5;
        } else if conventional(&word.text) { s[n-1].1 = 5; }
        let boundary = split(&word.text, jieba);
        let left: String = chars[..boundary].iter().collect();
        let right: String = chars[boundary..].iter().collect();
        if boundary > 0 && conventional(&left) { s[boundary-1].1 = 5; }
        if boundary < n && conventional(&right) { s[n-1].1 = 5; }
    }
    match n {
        2 if s.iter().all(|s| s.1 == 3) => s[0].1 = 2,
        3 => {
            let b = split(&word.text, jieba).clamp(1, 2);
            if s.iter().all(|s| s.1 == 3) {
                if b == 2 { s[0].1 = 2; }
                s[1].1 = 2;
            } else {
                if b == 2 && s[..b].iter().all(|s| s.1 == 3) { s[0].1 = 2; }
                if b == 1 && s[b..].iter().all(|s| s.1 == 3) { s[1].1 = 2; }
                else if !s[b..].iter().all(|s| s.1 == 3) && s[b].1 == 3 && s[b-1].1 == 3 { s[b-1].1 = 2; }
            }
        }
        4 => { for i in [0,2] { if s[i].1 == 3 && s[i+1].1 == 3 { s[i].1 = 2; } } }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tones(text: &str, ts: &[u8]) -> Vec<u8> {
        let mut w = Word { text: text.into(), pos: "n".into(), syllables: ts.iter().map(|t| (String::new(), *t)).collect() };
        modify(&mut w, &super::super::g2p::test_jieba());
        w.syllables.iter().map(|s| s.1).collect()
    }
    #[test]
    fn search_tokens_split_by_character_length_and_word_text() {
        let mut jieba = Jieba::empty();
        jieba.add_word("所有", Some(100), Some("b"));
        assert_eq!(split("所有人", &jieba), 2);
        assert_eq!(split("人所有", &jieba), 1);
        assert_eq!(split("", &jieba), 0);
        // Search mode includes the shorter dictionary match and the full word.
        jieba.add_word("所有人", Some(1000), Some("n"));
        assert_eq!(split("所有人", &jieba), 2);
    }

    #[test] fn source_sandhi_table() {
        assert_eq!(tones("你好", &[3,3]), [2,3]);
        assert_eq!(tones("不怕", &[4,4]), [2,4]);
        assert_eq!(tones("看不懂", &[4,4,3]), [4,5,3]);
        assert_eq!(tones("一天", &[1,1]), [4,1]);
        assert_eq!(tones("一段", &[1,4]), [2,4]);
        assert_eq!(tones("第一", &[4,1]), [4,1]);
        assert_eq!(tones("看一看", &[4,1,4]), [4,5,4]);
        assert_eq!(tones("一零零", &[1,2,2]), [1,2,2]);
        assert_eq!(tones("所有人", &[3,3,2]), [2,3,2]);
    }

    #[test]
    fn numeral_sequences_keep_yi_and_repeated_digit_tones() {
        for (text, input) in [
            ("二一零", [4,1,2]),
            ("一壹壹", [1,1,1]),
            ("一〇〇", [1,2,2]),
            ("一１１", [1,1,1]),
            ("一百一", [1,3,1]),
        ] {
            assert_eq!(tones(text, &input), input, "{text}");
        }
        assert_eq!(tones("三个", &[1,4]), [1,5]);
        assert_eq!(tones("奶奶", &[3,3]), [3,5]);
    }
}
