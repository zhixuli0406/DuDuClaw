//! Chinese script-variant helpers for titles, generated memory and channel prose.
//!
//! Detection uses the strict generated SIMPLIFIED_ONLY set, the hand-reviewed
//! SIMP_TO_TRAD keys and AMBIGUOUS_SIMPLIFIED, plus SIMPLIFIED_PHRASES.
//! Runs without a detection trigger remain byte-identical. Once triggered,
//! OpenCC converts the whole prose run with phrase-aware S2TW dictionaries,
//! without regional vocabulary substitution (軟件 and 信息 stay that way).
//! Mixed-script text can have shared characters reinterpreted by phrase context,
//! e.g. 了解 → 瞭解; a wrong phrase guess remains possible.
//!
//! TAIWAN_VARIANTS normalizes 臺 → 台 and 賬 → 帳 after conversion, but only
//! when the original input did not contain that source character: user-typed
//! forms are retained throughout the input. The converter is initialized once;
//! initialization failure logs once and falls back to the incomplete legacy
//! character table, leaving ambiguous characters unchanged.
//!
//! `to_traditional_preserving_literals` preserves code, URLs and Markdown link
//! destinations. Callers skip kana-bearing Japanese text where appropriate.
//! This is a safety net behind the language prompt, not a language translator.

use std::collections::HashSet;
use std::sync::OnceLock;
use ferrous_opencc::{OpenCC, config::BuiltinConfig};

/// Curated Simplified → Traditional character map, sorted by the Simplified
/// character so lookups can binary-search. Only unambiguous one-to-one
/// mappings live here.
const SIMP_TO_TRAD: &[(char, char)] = &[
    ('万', '萬'), ('与', '與'), ('业', '業'), ('东', '東'), ('个', '個'), ('为', '為'), ('丽', '麗'), ('么', '麼'),
    ('义', '義'), ('习', '習'), ('乡', '鄉'), ('书', '書'), ('买', '買'), ('争', '爭'), ('亏', '虧'), ('产', '產'),
    ('亲', '親'), ('从', '從'), ('们', '們'), ('价', '價'), ('优', '優'), ('会', '會'), ('传', '傳'), ('体', '體'),
    ('侦', '偵'), ('债', '債'), ('储', '儲'), ('儿', '兒'), ('兑', '兌'), ('关', '關'), ('养', '養'), ('册', '冊'),
    ('农', '農'), ('决', '決'), ('况', '況'), ('冻', '凍'), ('净', '淨'), ('凉', '涼'), ('减', '減'), ('凤', '鳳'),
    ('击', '擊'), ('则', '則'), ('删', '刪'), ('剂', '劑'), ('务', '務'), ('动', '動'), ('励', '勵'), ('势', '勢'),
    ('区', '區'), ('医', '醫'), ('华', '華'), ('单', '單'), ('卖', '賣'), ('卫', '衛'), ('厂', '廠'), ('厅', '廳'),
    ('压', '壓'), ('县', '縣'), ('双', '雙'), ('变', '變'), ('号', '號'), ('启', '啟'), ('员', '員'), ('响', '響'),
    ('团', '團'), ('园', '園'), ('围', '圍'), ('国', '國'), ('图', '圖'), ('场', '場'), ('块', '塊'), ('声', '聲'),
    ('处', '處'), ('备', '備'), ('够', '夠'), ('头', '頭'), ('夹', '夾'), ('奖', '獎'), ('妇', '婦'), ('孙', '孫'),
    ('学', '學'), ('实', '實'), ('审', '審'), ('宾', '賓'), ('导', '導'), ('层', '層'), ('属', '屬'), ('岁', '歲'),
    ('币', '幣'), ('库', '庫'), ('应', '應'), ('废', '廢'), ('开', '開'), ('异', '異'), ('弃', '棄'), ('张', '張'),
    ('弹', '彈'), ('归', '歸'), ('录', '錄'), ('忆', '憶'), ('态', '態'), ('总', '總'), ('户', '戶'), ('执', '執'),
    ('扩', '擴'), ('护', '護'), ('报', '報'), ('择', '擇'), ('挡', '擋'), ('损', '損'), ('换', '換'), ('数', '數'),
    ('断', '斷'), ('旧', '舊'), ('时', '時'), ('显', '顯'), ('机', '機'), ('权', '權'), ('条', '條'), ('构', '構'),
    ('标', '標'), ('栈', '棧'), ('样', '樣'), ('档', '檔'), ('检', '檢'), ('楼', '樓'), ('汉', '漢'), ('洁', '潔'),
    ('测', '測'), ('浏', '瀏'), ('润', '潤'), ('温', '溫'), ('湿', '濕'), ('满', '滿'), ('灭', '滅'), ('灯', '燈'),
    ('灵', '靈'), ('点', '點'), ('烟', '煙'), ('热', '熱'), ('爱', '愛'), ('状', '狀'), ('狱', '獄'), ('猪', '豬'),
    ('献', '獻'), ('现', '現'), ('电', '電'), ('疗', '療'), ('监', '監'), ('盖', '蓋'), ('盘', '盤'), ('矿', '礦'),
    ('码', '碼'), ('确', '確'), ('稳', '穩'), ('竞', '競'), ('简', '簡'), ('类', '類'), ('纠', '糾'), ('约', '約'),
    ('级', '級'), ('纪', '紀'), ('纷', '紛'), ('线', '線'), ('组', '組'), ('织', '織'), ('终', '終'), ('经', '經'),
    ('结', '結'), ('络', '絡'), ('继', '繼'), ('绩', '績'), ('续', '續'), ('维', '維'), ('缆', '纜'), ('缉', '緝'),
    ('缓', '緩'), ('编', '編'), ('缩', '縮'), ('网', '網'), ('罚', '罰'), ('职', '職'), ('脑', '腦'), ('脸', '臉'),
    ('苏', '蘇'), ('荣', '榮'), ('药', '藥'), ('营', '營'), ('虑', '慮'), ('补', '補'), ('装', '裝'), ('规', '規'),
    ('视', '視'), ('览', '覽'), ('触', '觸'), ('誉', '譽'), ('计', '計'), ('认', '認'), ('讨', '討'), ('让', '讓'),
    ('议', '議'), ('讯', '訊'), ('记', '記'), ('许', '許'), ('论', '論'), ('讼', '訟'), ('设', '設'), ('访', '訪'),
    ('证', '證'), ('评', '評'), ('识', '識'), ('诉', '訴'), ('译', '譯'), ('试', '試'), ('询', '詢'), ('该', '該'),
    ('语', '語'), ('误', '誤'), ('说', '說'), ('请', '請'), ('谁', '誰'), ('调', '調'), ('谈', '談'), ('负', '負'),
    ('财', '財'), ('责', '責'), ('账', '帳'), ('质', '質'), ('购', '購'), ('贴', '貼'), ('贷', '貸'), ('贸', '貿'),
    ('费', '費'), ('资', '資'), ('赠', '贈'), ('赢', '贏'), ('车', '車'), ('转', '轉'), ('软', '軟'), ('载', '載'),
    ('辆', '輛'), ('辑', '輯'), ('输', '輸'), ('辩', '辯'), ('边', '邊'), ('达', '達'), ('迁', '遷'), ('过', '過'),
    ('这', '這'), ('进', '進'), ('违', '違'), ('连', '連'), ('选', '選'), ('递', '遞'), ('遗', '遺'), ('邮', '郵'),
    ('释', '釋'), ('钢', '鋼'), ('钥', '鑰'), ('钱', '錢'), ('铁', '鐵'), ('铜', '銅'), ('铝', '鋁'), ('银', '銀'),
    ('销', '銷'), ('锁', '鎖'), ('错', '錯'), ('键', '鍵'), ('镇', '鎮'), ('长', '長'), ('门', '門'), ('闭', '閉'),
    ('问', '問'), ('间', '間'), ('阅', '閱'), ('队', '隊'), ('阳', '陽'), ('阴', '陰'), ('际', '際'), ('险', '險'),
    ('隐', '隱'), ('静', '靜'), ('页', '頁'), ('项', '項'), ('预', '預'), ('领', '領'), ('频', '頻'), ('题', '題'),
    ('颜', '顏'), ('饮', '飲'), ('馆', '館'), ('马', '馬'), ('驱', '驅'), ('验', '驗'), ('鱼', '魚'), ('鲜', '鮮'),
    ('鸟', '鳥'), ('鸡', '雞'), ('鸭', '鴨'), ('齿', '齒'), ('龄', '齡'), ('龙', '龍'),
];

/// Simplified characters whose Traditional counterpart depends on the word.
/// Every member is a codepoint zh-TW writers do not type. OpenCC resolves
/// these by phrase; the fallback leaves them alone (sorted).
///
/// 冲 沖/衝 · 别 別/彆 · 历 歷/曆 · 发 發/髮 · 复 復/複/覆 ·
/// 尽 盡/儘 · 汇 匯/彙 · 签 簽/籤 · 钟 鐘/鍾
///
/// Characters valid in BOTH scripts (游, 丰, 术, 据, 种, 后, 里, 只, 面, 干,
/// 云, 台, 表, 谷, 松, 板, 系, 制 …) appear in NEITHER table — they are
/// ordinary Traditional characters and must not be flagged individually, or
/// [`dominant_variant`] would misread 「上游 API」 as Simplified.
const AMBIGUOUS_SIMPLIFIED: &[char] = &['冲', '别', '历', '发', '复', '尽', '汇', '签', '钟'];

/// Detection-only data from ferrous-opencc 0.4.0's
/// `assets/dictionaries/STCharacters.txt` (OpenCC, Apache-2.0).
/// A character c is included only if (a) c is a single-character key, (b) c never
/// occurs in ANY target alternative of that dictionary, and (c) S2TW(c) != c.
/// To regenerate, read that file under
/// `~/.cargo/registry/src/*/ferrous-opencc-0.4.0/`, collect all target characters
/// across space-separated alternatives, subtract them from the single-char keys,
/// then filter with OpenCC::from_config(BuiltinConfig::S2tw).convert(c) != c.
/// Retain the existing shared-character exclusions 游丰术据种后里只面干云台
/// (notably 术 in 白术), then sort by Unicode scalar value and emit chunks below. For single characters,
/// the shipped STPhrases/STCharacters first targets followed by TWVariants first
/// targets reproduce S2TW; the test below checks condition (c) with OpenCC itself.
const SIMPLIFIED_ONLY: &str = concat!(
    "㐷㐽㑇㑈㑔㑩㓆㓥㓰㔉㖊㖞㘎㚯㛀㛟㛠㛣㛤㛿㟆㟜㟥㡎㤘㤽㥪㧏㧐㧑㧟㧰㨫㭎㭏㭣㭤㭴㱩㱮㲿㳔㳕㳠㳡㳢㳽㴋㶉㶶㶽㺍㻅㻏㻘䀥䁖䂵䃅䅉䅟䅪䇲䉤䌶䌷䌸䌹䌺䌻䌼䌽䌾䌿䍀䍁䍠䎬䏝䑽䓓䓕䓖䓨䗖䘛䘞䙊䙌䙓䜣䜤䜥䜧䜩䝙䞌䞍䞎䞐",
    "䟢䢀䢁䢂䥺䥽䥾䥿䦀䦁䦂䦃䦅䦆䦶䦷䩄䭪䯃䯄䯅䲝䲞䲟䲠䲡䲢䲣䴓䴔䴕䴖䴗䴘䴙䶮与专业丛东丝丢两严丧个临为丽举么义乌乐乔习乡书买乱争亏亚产亩亲亵亸亿仅从仑仓仪们众优会伛伞伟传伡伣伤伥伦伧伪伫体佥侠侣侥侦侧侨侩",
    "侪侬侭俣俦俨俩俪俫俭债倾偬偻偾偿傤傥傧储傩儿兑兖兰关兴兹养兽冁内冈册写军农冯冲决况冻净凄凉减凑凛凤凫凭凯击凿刍刘则刚创删别刬刭刹刽刾刿剀剂剐剑剥剧劝办务劢动励劲劳势勋勚匀匦匮区医华协单卖卢卤卧卫却卺厅",
    "历厉压厌厍厐厕厢厣厦厨厩厮县叁参叆叇双发变叙叠号叹叽吓吕吗吨听启吴呐呒呓呕呖呗员呙呛呜咏咙咛咝咤响哑哒哓哔哕哗哙哜哝哟唛唝唠唡唢唤啧啬啭啮啯啰啴啸喷喽喾嗫嗳嘘嘤嘱噜嚣团园囱围囵国图圆圣圹场坏块坚坛坜坝",
    "坞坟坠垄垅垆垒垦垩垫垭垯垱垲垴埘埙埚堑堕塆墙壮声壳壶壸处备复够头夹夺奁奂奋奖奥妆妇妈妩妪妫姗姹娄娅娆娇娈娱娲娴婳婴婵婶媪媭嫒嫔嫱嬷孙学孪宁宝实宠审宪宫宽宾寝对寻导寿将尔尘尝尧尴尽层屃屉届属屡屦屿岁岂岖",
    "岗岘岚岛岭岽岿峃峄峡峣峤峥峦崂崃崄崭嵘嵚嵝巅巩巯币帅师帏帐帜带帧帮帱帻帼幂并庄庆庐庑库应庙庞废庼廪开异弃弑张弥弪弯弹强归当录彟彦彨彻径徕忆忏忧忾怀态怂怃怄怅怆怜总怼怿恋恒恳恶恸恹恺恻恼恽悦悫悬悭悮悯惊",
    "惧惨惩惫惬惭惮惯愠愤愦慑慭懑懒懔戆戋戏戗战戬戯户扑执扩扪扫扬扰抚抛抟抠抡抢护报担拟拢拣拥拦拧拨择挚挛挜挝挞挟挠挡挢挣挤挥挦捝捞损捡换捣掳掴掷掸掺掼揽揾揿搀搁搂搄搅携摄摅摆摇摈摊撄撑撵撷撸撺擜擞攒敌敚敛",
    "敩数斋斓斩断无旧时旷旸昙昵昼昽显晋晒晓晔晕晖暂暅暧机杀杂权条来杨杩构枞枢枣枥枧枨枪枫枭柠柽栀栅标栈栉栊栋栌栎栏树栖样栾桠桡桢档桤桥桦桧桨桩桪梦梼梾梿检棁棂椁椝椟椠椢椤椫椭椮楼榄榅榇榈榉榝槚槛槟槠横樯",
    "樱橥橱橹橼檩欢欤欧歼殁殇残殒殓殚殡殴毁毂毕毙毡毵毶氇气氢氩氲汇汉汤汹沄沟没沣沤沥沦沧沨沩沪泞泪泶泷泸泺泻泼泽泾洁洒洼浃浅浆浇浈浉浊测浍济浏浐浑浒浓浔浕涚涛涝涞涟涠涡涢涣涤润涧涨涩渊渌渍渎渐渑渔渖渗温湾",
    "湿溁溃溅溆溇滗滚滞滟滠满滢滤滥滦滨滩滪潆潇潋潍潜潴澛澜濑濒灏灭灯灵灾灿炀炉炖炜炝点炼炽烁烂烃烛烟烦烧烨烩烫烬热焕焖焘煴爱爷牍牦牵牺犊状犷犸犹狈狝狞独狭狮狯狰狱狲猃猎猕猡猪猫猬献獭玑玙玚玛玮环现玱玺珐珑",
    "珰珲琎琏琐琼瑶瑷瑸璎瓒瓮瓯电画畅畴疖疗疟疠疡疬疭疮疯疱疴痈痉痒痖痨痪痫瘅瘆瘗瘘瘪瘫瘾瘿癞癣癫皑皱皲盏盐监盖盗盘眍眦眬睁睐睑瞆瞒瞩矫矶矾矿砀码砖砗砚砜砺砻砾础硁硕硖硗硙硚硵硷碍碛碜碱礼祃祎祢祯祷祸禀禄禅",
    "离秃秆积称秽秾稆税稣稳穑穞穷窃窍窎窑窜窝窥窦窭竖竞笃笋笔笕笺笼笾筚筛筜筝筹筼签筿简箓箦箧箨箩箪箫篑篓篮篯篱簖籁籴类籼粜粝粤粪粮糁糇糍紧絷緼縆纟纠纡红纣纤纥约级纨纩纪纫纬纭纮纯纰纱纲纳纴纵纶纷纸纹纺纻纼",
    "纽纾线绀绁绂练组绅细织终绉绊绋绌绍绎经绐绑绒结绔绕绖绗绘给绚绛络绝绞统绠绡绢绣绤绥绦继绨绩绪绫绬续绮绯绰绱绲绳维绵绶绷绸绹绺绻综绽绾绿缀缁缂缃缄缅缆缇缈缉缊缋缌缍缎缏缐缑缒缓缔缕编缗缘缙缚缛缜缝缞缟缠",
    "缡缢缣缤缥缦缧缨缩缪缫缬缭缮缯缰缱缲缳缴缵罂网罗罚罢罴羁羟羡翘翙翚耢耧耸耻聂聋职聍联聩聪肃肠肤肮肴肾肿胀胁胆胧胨胪胫胶脉脍脏脐脑脓脔脚脱脶脸腘腭腻腼腽腾膑臜舆舣舰舱舻艰艳艺节芈芗芜芦苁苇苈苋苌苍苎苏茎",
    "茏茑茔茕茧荆荙荚荛荜荝荞荟荠荡荣荤荥荦荧荨荩荪荫荬荭荮药莅莱莲莳莴莶获莸莹莺莼萚萝萤营萦萧萨葱蒀蒇蒉蒋蒌蒏蓝蓟蓠蓣蓥蓦蔂蔷蔹蔺蔼蕰蕲蕴薮藓藴蘖虏虑虚虬虮虱虽虾虿蚀蚁蚂蚃蚕蚬蛊蛎蛏蛮蛰蛱蛲蛳蛴蜕蜗蝇蝈蝉",
    "蝼蝾螀螨蟏衅衔补衬衮袄袅袆袜袭袯装裆裈裢裣裤裥褛褴襕见观觃规觅视觇览觉觊觋觌觍觎觏觐觑觞触觯訚詟誉誊讠计订讣认讥讦讧讨让讪讫讬训议讯记讱讲讳讴讵讶讷许讹论讻讼讽设访诀证诂诃评诅识诇诈诉诊诋诌词诎诏诐译",
    "诒诓诔试诖诗诘诙诚诛诜话诞诟诠诡询诣诤该详诧诨诩诪诫诬语诮误诰诱诲诳说诵诶请诸诹诺读诼诽课诿谀谁谂调谄谅谆谇谈谉谊谋谌谍谎谏谐谑谒谓谔谕谖谗谘谙谚谛谜谝谞谟谠谡谢谣谤谥谦谧谨谩谪谫谬谭谮谯谰谱谲谳谴谵",
    "谶豮贝贞负贠贡财责贤败账货质贩贪贫贬购贮贯贰贱贲贳贴贵贶贷贸费贺贻贼贽贾贿赀赁赂赃资赅赆赇赈赉赊赋赌赍赎赏赐赑赒赓赔赕赖赗赘赙赚赛赜赝赞赟赠赡赢赣赪赵赶趋趱趸跃跄跞践跶跷跸跹跻踌踪踬踯蹑蹒蹰蹿躏躜躯輼",
    "车轧轨轩轪轫转轭轮软轰轱轲轳轴轵轶轷轸轹轺轻轼载轾轿辀辁辂较辄辅辆辇辈辉辊辋辌辍辎辏辐辑辒输辔辕辖辗辘辙辚辞辩辫边辽达迁过迈运还这进远违连迟迩迳迹选逊递逦逻遗遥邓邝邬邮邹邺邻郏郐郑郓郦郧郸酂酝酦酱酽酾",
    "酿醖释鉴銮錾钅钆钇针钉钊钋钌钍钎钏钐钑钒钓钔钕钖钗钘钙钚钛钜钝钞钟钠钡钢钣钤钥钦钧钨钩钪钫钬钭钮钯钰钱钲钳钴钵钶钷钸钹钺钻钼钽钾钿铀铁铂铃铄铅铆铇铈铉铊铋铌铍铎铏铐铑铒铓铔铕铖铗铘铙铚铛铜铝铞铟铠铡铢",
    "铣铤铥铦铧铨铩铪铫铬铭铮铯铰铱铲铳铴铵银铷铸铹铺铻铼铽链铿销锁锂锃锄锅锆锇锈锉锊锋锌锍锎锏锐锑锒锓锔锕锖锗锘错锚锛锜锝锞锟锠锡锢锣锤锥锦锧锨锩锪锫锬锭键锯锰锱锲锳锴锵锶锷锸锹锺锻锼锽锾锿镀镁镂镃镄镅镆",
    "镇镈镉镊镋镌镍镎镏镐镑镒镓镔镕镖镗镘镙镚镛镜镝镞镟镠镡镢镣镤镥镦镧镨镩镪镫镬镭镮镯镰镱镲镳镴镵镶长门闩闪闫闬闭问闯闰闱闲闳间闵闶闷闸闹闺闻闼闽闾闿阀阁阂阃阄阅阆阇阈阉阊阋阌阍阎阏阐阑阒阓阔阕阖阗阘阙阚",
    "阛队阳阴阵阶际陆陇陈陉陕陦陧陨险随隐隶隽难雇雏雠雳雾霁霉霡霭靓靔静靥鞑鞒鞯鞲韦韧韨韩韪韫韬韵页顶顷顸项顺须顼顽顾顿颀颁颂颃预颅领颇颈颉颊颋颌颍颎颏颐频颒颓颔颕颖颗题颙颚颛颜额颞颟颠颡颢颣颤颥颦颧风飏飐",
    "飑飒飓飔飕飖飗飘飙飚飞飨餍饣饤饥饦饧饨饩饪饫饬饭饮饯饰饱饲饳饴饵饶饷饸饹饺饻饼饽饾饿馀馁馂馃馄馅馆馇馈馉馊馋馌馍馎馏馐馑馒馓馔馕马驭驮驯驰驱驲驳驴驵驶驷驸驹驺驻驼驽驾驿骀骁骂骃骄骅骆骇骈骉骊骋验骍骎骏",
    "骐骑骒骓骔骕骖骗骘骙骚骛骜骝骞骟骠骡骢骣骤骥骦骧髅髋髌鬓鬶魇魉鱼鱽鱾鱿鲀鲁鲂鲃鲄鲅鲆鲇鲈鲉鲊鲋鲌鲍鲎鲏鲐鲑鲒鲓鲔鲕鲖鲗鲘鲙鲚鲛鲜鲝鲞鲟鲠鲡鲢鲣鲤鲥鲦鲧鲨鲩鲪鲫鲬鲭鲮鲯鲰鲱鲲鲳鲴鲵鲶鲷鲸鲹鲺鲻鲼鲽鲾鲿鳀",
    "鳁鳂鳃鳄鳅鳆鳇鳈鳉鳊鳋鳌鳍鳎鳏鳐鳑鳒鳓鳔鳕鳖鳗鳘鳙鳚鳛鳜鳝鳞鳟鳠鳡鳢鳣鳤鸟鸠鸡鸢鸣鸤鸥鸦鸧鸨鸩鸪鸫鸬鸭鸮鸯鸰鸱鸲鸳鸴鸵鸶鸷鸸鸹鸺鸻鸼鸽鸾鸿鹀鹁鹂鹃鹄鹅鹆鹇鹈鹉鹊鹋鹌鹍鹎鹏鹐鹑鹒鹓鹔鹕鹖鹗鹘鹙鹚鹛鹜鹝鹞",
    "鹟鹠鹡鹢鹣鹤鹥鹦鹧鹨鹩鹪鹫鹬鹭鹮鹯鹰鹱鹲鹳鹴鹾麦麸麹麺麽黄黉黡黩黪黾鼋鼌鼍鼹齐齑齿龀龁龂龃龄龅龆龇龈龉龊龋龌龙龚龛龟鿎鿏鿒鿔𠀾𠆲𠆿𠇹𠉂𠉗𠋆𠚳𠛅𠛆𠛾𠡠𠮶𠯟𠯠𠰱𠰷𠱞𠲥𠴛𠴢𠵸𠵾𡋀𡋗𡋤𡍣𡒄𡝠𡞋𡞱𡠟𡥧𡭜𡭬𡳃𡳒𡶴",
    "𡸃𡺃𡺄𢋈𢗓𢘙𢘝𢘞𢙏𢙐𢙑𢙒𢙓𢛯𢠁𢢐𢧐𢫊𢫞𢫬𢬍𢬦𢭏𢽾𣃁𣆐𣈣𣍨𣍯𣍰𣎑𣏢𣐕𣐤𣑶𣒌𣓿𣔌𣗊𣗋𣗙𣘐𣘓𣘴𣘷𣚚𣞎𣨼𣭤𣯣𣱝𣲗𣲘𣳆𣶩𣶫𣶭𣷷𣸣𣺼𣺽𣽷𤆡𤆢𤇃𤇄𤇭𤇹𤈶𤈷𤊀𤊰𤋏𤎺𤎻𤙯𤝢𤞃𤞤𤠋𤦀𤩽𤳄𤶊𤶧𤻊𤽯𤾀𤿲𥁢𥅘𥅴𥅿𥆧𥇢𥎝𥐟𥐯𥐰𥐻",
    "𥞦𥧂𥩟𥩺𥫣𥬀𥬞𥬠𥭉𥮋𥮜𥮾𥱔𥹥𥺅𥺇𦈈𦈉𦈋𦈌𦈎𦈏𦈐𦈑𦈒𦈓𦈔𦈕𦈖𦈗𦈘𦈙𦈚𦈛𦈜𦈝𦈞𦈟𦈠𦈡𦍠𦛨𦝼𦟗𦨩𦰏𦰴𦶟𦶻𦻕𧉐𧉞𧌥𧏖𧏗𧑏𧒭𧜭𧝝𧝧𧮪𧳕𧹑𧹒𧹓𧹔𧹕𧹖𧹗𧿈𨀁𨀱𨁴𨂺𨄄𨅛𨅫𨅬𨉗𨐅𨐆𨐇𨐈𨐉𨐊𨑹𨟳𨠨𨡙𨡺𨤰𨰾𨰿𨱀𨱁𨱂𨱃𨱄𨱅𨱆",
    "𨱇𨱈𨱉𨱊𨱋𨱌𨱍𨱎𨱏𨱐𨱑𨱒𨱓𨱔𨱕𨱖𨷿𨸀𨸁𨸂𨸃𨸄𨸅𨸆𨸇𨸉𨸊𨸋𨸌𨸎𨸘𨸟𩏼𩏽𩏾𩏿𩐀𩓋𩖕𩖖𩖗𩙥𩙦𩙧𩙨𩙩𩙪𩙫𩙬𩙭𩙮𩙯𩙰𩟿𩠀𩠁𩠂𩠃𩠅𩠆𩠇𩠈𩠉𩠊𩠋𩠌𩠎𩠏𩠠𩡖𩧦𩧨𩧩𩧪𩧫𩧬𩧭𩧮𩧯𩧰𩧱𩧲𩧳𩧴𩧵𩧶𩧸𩧺𩧻𩧼𩧿𩨀𩨁𩨂𩨃𩨄𩨅𩨆𩨇𩨈",
    "𩨉𩨊𩨋𩨌𩨍𩨎𩨏𩨐𩩈𩬣𩬤𩭹𩯒𩰰𩲒𩴌𩽹𩽺𩽻𩽼𩽽𩽾𩽿𩾁𩾂𩾃𩾄𩾅𩾆𩾇𩾈𩾊𩾋𩾌𩾎𪉂𪉃𪉄𪉅𪉆𪉈𪉉𪉊𪉋𪉌𪉍𪉎𪉏𪉐𪉑𪉒𪉔𪉕𪎈𪎉𪎊𪎋𪎌𪑅𪔭𪚏𪚐𪜎𪞝𪟎𪟝𪠀𪠟𪠡𪠳𪠵𪠸𪠺𪠽𪡀𪡃𪡋𪡏𪡛𪡞𪡺𪢌𪢐𪢒𪢕𪢖𪢠𪢮𪢸𪣆𪣒𪣻𪤄𪤚𪥠𪥫𪥰𪥿𪧀𪧘",
    "𪨊𪨗𪨧𪨩𪨶𪨷𪨹𪩇𪩎𪩘𪩛𪩷𪩸𪪏𪪑𪪞𪪴𪪼𪫌𪫡𪫷𪫺𪬚𪬯𪭝𪭢𪭧𪭯𪭵𪭾𪮃𪮋𪮖𪮳𪮶𪯋𪰶𪱥𪱷𪲎𪲔𪲛𪲮𪳍𪳗𪴙𪵑𪵣𪵱𪶄𪶒𪶮𪷍𪷽𪸕𪸩𪹀𪹠𪹳𪹹𪺣𪺪𪺭𪺷𪺸𪺻𪺽𪻐𪻨𪻲𪻺𪼋𪼴𪽈𪽝𪽪𪽭𪽮𪽴𪽷𪾔𪾢𪾣𪾦𪾸𪿊𪿞𪿫𪿵𫀌𫀓𫀨𫀬𫀮𫁂𫁟𫁡𫁱𫁲𫁳",
    "𫁷𫁺𫂃𫂆𫂈𫂖𫂿𫃗𫄙𫄚𫄛𫄜𫄝𫄞𫄟𫄠𫄡𫄢𫄣𫄤𫄥𫄦𫄧𫄨𫄩𫄪𫄫𫄬𫄭𫄮𫄯𫄰𫄱𫄲𫄳𫄴𫄵𫄶𫄷𫄸𫄹𫅅𫅗𫅥𫅭𫅼𫆏𫆝𫆫𫇘𫇛𫇪𫇭𫇴𫇽𫈉𫈎𫈟𫈵𫉁𫉄𫊪𫊮𫊸𫊹𫊻𫋇𫋌𫋲𫋷𫋹𫋻𫌀𫌇𫌋𫌨𫌪𫌫𫌬𫌭𫌯𫍐𫍙𫍚𫍛𫍜𫍝𫍞𫍟𫍠𫍡𫍢𫍣𫍤𫍥𫍦𫍧𫍨𫍩𫍪",
    "𫍫𫍬𫍭𫍮𫍯𫍰𫍱𫍲𫍳𫍴𫍵𫍶𫍷𫍸𫍹𫍺𫍻𫍼𫍽𫍾𫍿𫎆𫎌𫎦𫎧𫎨𫎩𫎪𫎫𫎬𫎭𫎱𫎳𫎸𫎺𫏃𫏆𫏋𫏌𫏐𫏑𫏕𫏞𫏨𫐄𫐅𫐆𫐇𫐈𫐉𫐊𫐋𫐌𫐍𫐎𫐏𫐐𫐑𫐒𫐓𫐔𫐕𫐖𫐗𫐘𫐙𫐷𫑘𫑡𫑷𫓥𫓦𫓧𫓨𫓩𫓪𫓫𫓬𫓭𫓮𫓯𫓰𫓱𫓲𫓳𫓴𫓵𫓶𫓷𫓸𫓹𫓺𫓻𫓼𫓽𫓾𫓿𫔀𫔁𫔂",
    "𫔃𫔄𫔅𫔆𫔇𫔈𫔉𫔊𫔋𫔌𫔍𫔎𫔏𫔐𫔑𫔒𫔓𫔔𫔕𫔖𫔭𫔮𫔯𫔰𫔲𫔴𫔵𫔶𫔽𫕚𫕥𫕨𫖃𫖅𫖇𫖑𫖒𫖓𫖔𫖕𫖖𫖪𫖫𫖬𫖭𫖮𫖯𫖰𫖱𫖲𫖳𫖴𫖵𫖶𫖷𫖸𫖹𫖺𫗇𫗈𫗉𫗊𫗋𫗚𫗞𫗟𫗠𫗡𫗢𫗣𫗤𫗥𫗦𫗧𫗨𫗩𫗪𫗫𫗬𫗭𫗮𫗯𫗰𫗱𫗳𫗴𫗵𫘛𫘜𫘝𫘞𫘟𫘠𫘡𫘣𫘤𫘥𫘦𫘧𫘨",
    "𫘩𫘪𫘫𫘬𫘭𫘮𫘯𫘰𫘱𫘽𫙂𫚈𫚉𫚊𫚋𫚌𫚍𫚎𫚏𫚐𫚑𫚒𫚓𫚔𫚕𫚖𫚗𫚘𫚙𫚚𫚛𫚜𫚝𫚞𫚟𫚠𫚡𫚢𫚣𫚤𫚥𫚦𫚧𫚨𫚩𫚪𫚫𫚬𫚭𫛚𫛛𫛜𫛝𫛞𫛟𫛠𫛡𫛢𫛣𫛤𫛥𫛦𫛧𫛨𫛩𫛪𫛫𫛬𫛭𫛮𫛯𫛰𫛱𫛲𫛳𫛴𫛵𫛶𫛷𫛸𫛹𫛺𫛻𫛼𫛽𫛾𫜀𫜁𫜂𫜃𫜄𫜅𫜊𫜑𫜒𫜓𫜔𫜕𫜙𫜟",
    "𫜨𫜩𫜪𫜫𫜬𫜭𫜮𫜯𫜰𫜲𫜳𫝈𫝋𫝦𫝧𫝨𫝩𫝪𫝫𫝬𫝭𫝮𫝵𫞅𫞗𫞚𫞛𫞝𫞠𫞡𫞢𫞣𫞥𫞦𫞧𫞨𫞩𫞷𫟃𫟄𫟅𫟆𫟇𫟑𫟕𫟞𫟟𫟠𫟡𫟢𫟤𫟥𫟦𫟫𫟬𫟲𫟳𫟴𫟵𫟶𫟷𫟸𫟹𫟺𫟻𫟼𫟽𫟾𫟿𫠀𫠁𫠂𫠅𫠆𫠇𫠈𫠊𫠋𫠌𫠏𫠐𫠑𫠒𫠖𫠜𫢸𫧃𫧮𫫇𫬐𫭟𫭢𫭼𫮃𫰛𫵷𫶇𫷷𫸩𬀩",
    "𬀪𬂩𬃊𬇕𬇙𬇹𬉼𬊈𬊤𬍛𬍡𬍤𬒈𬒗𬕂𬘓𬘘𬘡𬘩𬘫𬘬𬘭𬘯𬙂𬙊𬙋𬜬𬜯𬞟𬟁𬟽𬣙𬣞𬣡𬣳𬤇𬤊𬤝𬨂𬨎𬩽𬪩𬬩𬬭𬬮𬬱𬬸𬬹𬬻𬬿𬭁𬭊𬭎𬭚𬭛𬭤𬭩𬭬𬭭𬭯𬭳𬭶𬭸𬭼𬮱𬮿𬯀𬯎𬱖𬱟𬳵𬳶𬳽𬳿𬴂𬴃𬴊𬶋𬶍𬶏𬶐𬶟𬶠𬶨𬶭𬶮𬷕𬸘𬸚𬸣𬸦𬸪𬸯𬹼𬺈𬺓𰬸𰰨𰶎𰾄",
    "𰾭𱊜",
);

/// These shared-character phrases signal Simplified usage despite having no
/// Simplified-only characters: zh-TW writes 裡面, 幹部 and 以後 respectively.
const SIMPLIFIED_PHRASES: &[&str] = &["里面", "干部", "以后"];

/// Preferred Taiwan output forms missing from S2TW. Apply only if the input
/// lacks the source form, so normalization never replaces a user-typed variant.
const TAIWAN_VARIANTS: &[(char, char)] = &[
    ('臺', '台'), // Retain the existing preference for 台 in generated output.
    ('賬', '帳'), // Taiwan uses 帳 for accounts and billing.
    ('溼', '濕'), // Taiwan everyday usage prefers 濕; preserve user-typed 溼.
];

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

/// Detect Simplified-only characters plus explicit shared-character phrases.
/// Shared characters such as 后, 里, 干 and 台 never trigger on their own.
pub fn contains_simplified(s: &str) -> bool {
    static CHARACTERS: OnceLock<HashSet<char>> = OnceLock::new();
    let characters = CHARACTERS.get_or_init(|| SIMPLIFIED_ONLY.chars().collect());
    s.chars().any(|c| characters.contains(&c) || trad_for(c).is_some()
        || AMBIGUOUS_SIMPLIFIED.binary_search(&c).is_ok())
        || SIMPLIFIED_PHRASES.iter().any(|phrase| s.contains(phrase))
}

fn converter() -> Option<&'static OpenCC> {
    static CONVERTER: OnceLock<Option<OpenCC>> = OnceLock::new();
    CONVERTER.get_or_init(|| match OpenCC::from_config(BuiltinConfig::S2tw) {
        Ok(converter) => Some(converter),
        Err(error) => {
            tracing::warn!(%error, "OpenCC initialization failed; using incomplete legacy Chinese conversion");
            None
        }
    }).as_ref()
}

/// Convert a whole detected Simplified run using phrase context and Taiwan variants.
/// No detection means no conversion: Traditional/ASCII input stays byte-identical.
/// Output length may differ; callers must enforce budgets after conversion.
pub fn to_traditional(s: &str) -> String {
    to_traditional_with_input(s, s)
}

// Literal-preserving conversion splits prose into runs, but variant protection
// must consider the original input, including literals and other prose runs.
fn to_traditional_with_input(s: &str, input: &str) -> String {
    if !contains_simplified(s) {
        return s.to_string();
    }
    match converter() {
        Some(converter) => {
            let converted = converter.convert(s);
            let mut protected_sources: Vec<(char, usize)> = TAIWAN_VARIANTS.iter()
                .map(|&(source, _)| (source, input.chars().filter(|&c| c == source).count()))
                .collect();
            converted.chars().map(|c| {
                if let Some((_, remaining)) = protected_sources.iter_mut().find(|entry| entry.0 == c) {
                    if *remaining > 0 {
                        *remaining -= 1;
                        return c;
                    }
                }
                TAIWAN_VARIANTS.iter()
                    .find(|&&(source, _)| c == source && !input.contains(source))
                    .map_or(c, |&(_, target)| target)
            }).collect()
        },
        None => legacy_to_traditional(s),
    }
}

fn legacy_to_traditional(s: &str) -> String {
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
    fn simplified_only_characters_change_under_s2tw() {
        let converter = OpenCC::from_config(BuiltinConfig::S2tw)
            .expect("embedded dictionaries must load");
        for c in SIMPLIFIED_ONLY.chars() {
            let input = c.to_string();
            assert_ne!(converter.convert(&input), input, "unchanged S2TW character: {c}");
        }
    }

    #[test]
    fn taiwan_traditional_text_is_never_touched() {
        let business = "各位同事好，本週的專案進度已整理完成，請各部門主管於週五下班前確認附件內容。業務團隊需要核對客戶名單、報價紀錄與合約期限，財務部則請確認發票號碼、付款日期及銀行帳戶。若有尚未收到的憑證，請先註明原因，再由承辦人聯繫供應商補件。下週一上午將召開跨部門會議，討論新產品的交貨安排、倉庫存量與客服回覆流程。會議資料請存放在共用資料夾，並保留原始檔案，方便後續查閱。行政同仁會協助預訂會議室與設備，也請大家提前提出需要支援的事項。出差申請與交通費用請依公司規定辦理，如有疑問可洽人事窗口。感謝大家配合，祝工作順利。";
        assert!(business.chars().count() >= 200);
        for input in [
            "社區的里長和里民在床邊討論群組、山峰、秘密、爐灶與粽子。",
            "了解，我只想知道面積和干擾的範圍。",
            "皇后區的咖啡店後面有一台腳踏車。",
            "人云亦云，才發現系統公告說明天休息。",
            "臺北與台北都有人寫，帳號和帳單請寄到公司。",
            "這個月的營業額、預算與負責人已經確認。",
            "這個價格很划算，週末我們去划船。",
            "他划了一根火柴，計畫明天出發。",
            "請幫我把檔案傳給張經理，並回覆客戶。",
            business,
        ] {
            assert!(!contains_simplified(input), "must not detect: {input}");
            assert_eq!(to_traditional(input), input, "must not convert: {input}");
            assert_eq!(to_traditional_preserving_literals(input), input,
                "must not convert prose: {input}");
        }
    }

    #[test]
    fn legacy_curated_pairs_keep_taiwan_forms() {
        // Explicitly reviewed alternatives; keep the legacy expectation visible.
        for &(source, legacy) in SIMP_TO_TRAD {
            assert_eq!(to_traditional(&source.to_string()), legacy.to_string(),
                "legacy pair: {source} → {legacy}");
        }
    }

    #[test]
    fn taiwan_variants_preserve_user_typed_sources() {
        // A Taiwan variant replacement is skipped when its source appears anywhere in the input.
        for (input, expected) in [
            ("臺灣的用户", "臺灣的用戶"),
            ("台湾的用户", "台灣的用戶"),
            ("臺灣与台湾", "臺灣與臺灣"),
            ("賬戶与账户", "賬戶與賬戶"),
            ("账户资料", "帳戶資料"),
            ("湿度太高", "濕度太高"),
            ("溼度與湿气", "溼度與溼氣"),
        ] {
            assert_eq!(to_traditional(input), expected);
            assert_eq!(to_traditional_preserving_literals(input), expected);
            assert_eq!(to_traditional(expected), expected);
        }
        assert_eq!(to_traditional_preserving_literals("臺灣 `literal` 台湾的用户"),
            "臺灣 `literal` 臺灣的用戶");
        assert_eq!(to_traditional_preserving_literals("`賬` 账户资料"), "`賬` 賬戶資料");
        for &(source, target) in TAIWAN_VARIANTS {
            let input = format!("{source}用户");
            assert_eq!(to_traditional(&input), format!("{source}用戶"));
            assert!(!contains_simplified(&source.to_string()));
            assert_ne!(source, target);
        }
    }

    #[test]
    fn shared_character_phrase_triggers_are_simplified_usage() {
        let taiwan_forms = ["裡面", "幹部", "以後"];
        assert_eq!(SIMPLIFIED_PHRASES.len(), taiwan_forms.len());
        for (&simplified, traditional) in SIMPLIFIED_PHRASES.iter().zip(taiwan_forms) {
            assert!(simplified.chars().all(|c| !contains_simplified(&c.to_string())));
            assert!(contains_simplified(simplified));
            assert_eq!(to_traditional(simplified), traditional);
            assert!(!contains_simplified(traditional));
            assert_eq!(to_traditional(traditional), traditional);
        }
    }

    #[test]
    fn phrase_conversion_regressions() {
        let cases = [
            ("以后所有回复都用简體中文。…營業時间从早上八點開始。", "以後所有回覆都用簡體中文。…營業時間從早上八點開始。"),
            ("用户要求：这个项目的预算是三万元，负责人是张经理，截止日期是下个月。", "用戶要求：這個項目的預算是三萬元，負責人是張經理，截止日期是下個月。"),
            ("头发", "頭髮"), ("发现", "發現"), ("皇后", "皇后"),
            ("后来", "後來"), ("里面", "裡面"), ("公里", "公里"),
            ("干净", "乾淨"), ("干部", "幹部"), ("复习", "複習"),
            ("恢复", "恢復"), ("日历", "日曆"), ("历史", "歷史"),
            ("软件", "軟件"), ("信息", "信息"), ("台湾", "台灣"), ("台北", "台北"),
            ("台灣的皇后區有一間咖啡店，裡面只有三隻貓。", "台灣的皇后區有一間咖啡店，裡面只有三隻貓。"),
            ("後台、公里、乾淨、頭髮", "後台、公里、乾淨、頭髮"),
            ("", ""), ("ASCII identifier_with_underscores", "ASCII identifier_with_underscores"),
            ("以后", "以後"), ("简间从预经万", "簡間從預經萬"),
            ("闹钟和轰鸣", "鬧鐘和轟鳴"),
            ("计划书", "計劃書"),
            ("划分区域", "劃分區域"),
        ];
        for (input, expected) in cases {
            let actual = to_traditional(input);
            assert_eq!(actual, expected, "input: {input}");
            assert_eq!(to_traditional(&actual), actual, "idempotence: {input}");
        }
    }

    #[test]
    fn detects_previously_missing_characters_individually() {
        for (input, expected) in [("简", "簡"), ("间", "間"), ("从", "從"),
            ("预", "預"), ("经", "經"), ("万", "萬"), ("闹", "鬧"), ("轰", "轟")] {
            assert!(contains_simplified(input), "must detect: {input}");
            assert_eq!(to_traditional(input), expected);
            assert_eq!(to_traditional(expected), expected);
        }
    }

    #[test]
    fn mixed_prose_preserves_literals_and_is_idempotent() {
        let input = "用户预算三万元 https://站/预算 `回复` /tmp/预算.txt C:\\资料\\预算.txt memory_budget 開始";
        let expected = "用戶預算三萬元 https://站/预算 `回复` /tmp/预算.txt C:\\资料\\预算.txt memory_budget 開始";
        let actual = to_traditional_preserving_literals(input);
        assert_eq!(actual, expected);
        assert_eq!(to_traditional_preserving_literals(&actual), actual);
    }

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
        assert_eq!(to_traditional("了解用户个人档案信息"), "瞭解用戶個人檔案信息");
        assert!(!contains_simplified("瞭解用戶個人檔案信息"));
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
            "臺灣的咖啡店",
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

    #[test]
    fn opencc_configs_have_observed_character_variants() {
        for (config, expected) in [(BuiltinConfig::S2t, "臺灣 臺北 軟件 信息 賬戶 裏面"),
            (BuiltinConfig::S2tw, "臺灣 臺北 軟件 信息 賬戶 裡面")] {
            let converter = OpenCC::from_config(config).expect("embedded dictionaries must load");
            assert_eq!(converter.convert("台湾 台北 软件 信息 账户 里面"), expected);
        }
        assert!(converter().is_some(), "tests must exercise OpenCC, not the fallback");
    }

    #[test]
    fn legacy_fallback_remains_available() {
        assert_eq!(legacy_to_traditional("用户头发简间从预经万"), "用戶頭发簡間從預經萬");
    }

    #[test]
    fn account_char_uses_the_taiwan_form() {
        assert_eq!(to_traditional("账户资料"), "帳戶資料");
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

/// Kana marks Japanese text, including half-width Katakana.
pub fn contains_kana(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9f}'))
}

/// Convert prose while preserving code spans, fenced blocks, URLs and explicit paths.
pub fn to_traditional_preserving_literals(text: &str) -> String {
    let mut output = String::new();
    let mut fence: Option<(char, usize)> = None;
    let mut inline = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().unwrap_or(' ');
        let run = trimmed.chars().take_while(|c| *c == marker).count();
        if let Some((open, length)) = fence {
            output.push_str(line);
            if marker == open && run >= length && trimmed.chars().skip(run).all(char::is_whitespace) {
                fence = None;
            }
            continue;
        }
        if inline == 0 && matches!(marker, '`' | '~') && run >= 3 {
            fence = Some((marker, run));
            output.push_str(line);
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut prose = String::new();
        while i < chars.len() {
            if chars[i] == '`' {
                output.push_str(&to_traditional_with_input(&prose, text));
                prose.clear();
                let count = chars.iter().skip(i).take_while(|c| **c == '`').count();
                if inline == 0 { inline = count; } else if inline == count { inline = 0; }
                output.extend(std::iter::repeat_n('`', count));
                i += count;
            } else if inline > 0 {
                output.push(chars[i]);
                i += 1;
            } else if chars[i] == '(' && i > 0 && chars[i - 1] == ']' {
                // Markdown destinations can also be relative URLs with CJK paths.
                output.push_str(&to_traditional_with_input(&prose, text));
                prose.clear();
                let mut depth = 0usize;
                let mut escaped = false;
                while i < chars.len() {
                    let c = chars[i];
                    output.push(c);
                    i += 1;
                    if !escaped {
                        if c == '(' { depth += 1; }
                        if c == ')' { depth = depth.saturating_sub(1); }
                    }
                    escaped = c == '\\' && !escaped;
                    if depth == 0 { break; }
                }
            } else {
                let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
                let scheme_len = if boundary && chars[i].is_ascii_alphabetic() {
                    chars.iter().skip(i).take_while(|c| c.is_ascii_alphanumeric() || matches!(**c, '+' | '-' | '.')).count()
                } else { 0 };
                let scheme_url = scheme_len > 0 && chars.iter().skip(i + scheme_len).copied().take(3).eq("://".chars());
                let other_url = ["mailto:", "www."].iter().any(|prefix| {
                    chars.len() - i >= prefix.len() && chars.iter().skip(i).zip(prefix.chars())
                        .all(|(left, right)| left.eq_ignore_ascii_case(&right))
                });
                // Explicit filesystem paths are literals too, including CJK components.
                let path_boundary = i == 0 || chars[i - 1].is_whitespace()
                    || matches!(chars[i - 1], '(' | '[' | '"' | '\'');
                let explicit_path = path_boundary && (chars[i] == '/'
                    || ["./", "../", "~/", "\\\\"].iter().any(|prefix| {
                        chars.iter().skip(i).copied().take(prefix.chars().count()).eq(prefix.chars())
                    })
                    || (chars[i].is_ascii_alphabetic() && chars.get(i + 1) == Some(&':')
                        && matches!(chars.get(i + 2), Some('/' | '\\'))));
                if scheme_url || other_url || explicit_path {
                    output.push_str(&to_traditional_with_input(&prose, text));
                    prose.clear();
                    while i < chars.len() && !chars[i].is_whitespace() {
                        output.push(chars[i]);
                        i += 1;
                    }
                } else {
                    prose.push(chars[i]);
                    i += 1;
                }
            }
        }
        output.push_str(&to_traditional_with_input(&prose, text));
    }
    output
}

#[cfg(test)]
mod literal_tests {
    use super::*;
    #[test]
    fn preserves_urls_code_and_fences() {
        let text = "用户 https://站/用户 `用户`\n```\n用户\n```\n记忆";
        assert_eq!(to_traditional_preserving_literals(text), "用戶 https://站/用户 `用户`\n```\n用户\n```\n記憶");
        assert_eq!(to_traditional_preserving_literals("用户 http://站/用户<档案 用户"), "用戶 http://站/用户<档案 用戶");
    }
    #[test]
    fn detects_full_and_half_width_kana() {
        for text in ["会います", "カタカナ", "ｶﾀｶﾅ"] { assert!(contains_kana(text)); }
        assert!(!contains_kana("用戶"));
    }
}
