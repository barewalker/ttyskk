//! SKK の状態機械。
//!
//! 未確定の文字は一切外へ出さない。確定した文字列だけを [`Response::commit`] に載せ、
//! 途中経過は [`Skk::preedit`] が返す区間列、候補は [`Skk::candidates`] に回す。
//! 解釈しなかったキーは [`Response::passthrough`] としてそのまま返す。

use crate::config::{Config, DynamicCompletion, Layout, Marker, OkuriMatch};
use crate::context::Context;
use crate::dict::{Candidate, Dict};

/// 括弧付き貼り付け (bracketed paste) の囲み。
///
/// 端末は貼り付けた内容をこの二つで挟んで送る。挟まれた中身は「打鍵」ではないので、
/// ローマ字変換にもモード切り替えにも回さず、丸ごと `Key::Paste` にまとめる。
///
/// 端末の約束事なので本来は切り出す側 (`input`) の持ち物だが、確定した文字列に続けて
/// 貼り付けを組み直すのがここ (`raw_bytes`) なので、定義もここに置いている。
pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// 端末が Shift+Tab に使うバイト列 (`CSI Z`)。
pub const SHIFT_TAB: &[u8] = b"\x1b[Z";
use crate::num;
use crate::romaji::{self, Romaji};
use crate::snippet;

/// TAB 補完で拾う見出し語の上限。多すぎると巡るのに手間がかかる。
const COMPLETIONS: usize = 64;

/// 動的補完で並べる見出し語の数 (`multiple`)。
///
/// 一行に収める前提なので、多くしても端に落ちる。打鍵ごとに目に入るものなので、
/// 一目で見渡せる数に留める。
const DYNAMIC_SHOWN: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Ascii,
    Hiragana,
    Katakana,
    HankakuKatakana,
    ZenkakuAscii,
}

impl Mode {
    fn is_kana(&self) -> bool {
        matches!(
            self,
            Mode::Hiragana | Mode::Katakana | Mode::HankakuKatakana
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// 変換していない状態。かなはそのまま確定して子へ送る。
    Direct,
    /// ▽ 見出し語を入力中。
    Composing,
    /// ▼ 候補を選択中。
    Selecting,
}

/// 押されたキー。
///
/// SKK が割り当てに使う分だけを名前で持ち、それ以外のエスケープ列は解釈せず
/// [`Key::Raw`] のまま素通しする。**名前のあるものは端末のバイト列を含まない**ので、
/// GUI の入力メソッドから組み立てるときも `Raw` を作る必要がない。
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Ctrl(u8),
    Enter,
    Backspace,
    Tab,
    /// Shift+Tab。端末は `CSI Z` として送る。
    ShiftTab,
    Esc,
    /// 解釈しないキーの、端末から届いたままのバイト列。
    ///
    /// 矢印や機能キーがここに入る。エンジンは中身を見ず、そのまま返すだけ。
    Raw(Vec<u8>),
    /// 括弧付き貼り付けで届いた中身 (開始・終了の列は含まない)。
    ///
    /// 貼り付けは「打鍵」ではないので、ローマ字変換にもモード切り替えにも回さない。
    /// 素朴に一文字ずつ流すと `hello` が `へ` + ASCII モードへの `l` になってしまう。
    Paste(Vec<u8>),
}

/// 重ね描きする区間の見た目。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// ▽ / ▼ の印と見出し語
    Reading,
    /// ▽ の見出し語のうち、カーソルが乗っている一文字。
    ///
    /// 重ね描きしている間は端末のカーソルを隠している (`render::Overlay::draw`) ので、
    /// 見出し語の中を動いている位置は文字の見た目でしか示せない。カーソルを末尾から
    /// 動かしていないときは区間そのものが作られない。
    ReadingCursor,
    /// かなになっていないローマ字
    Romaji,
    /// 選択中の候補
    Candidate,
    /// 候補一覧の項目
    ListItem,
    /// 候補一覧で選択中の項目
    ListSelected,
    /// 打つそばから見せている補完。**まだ打っていない文字**。
    ///
    /// 候補一覧と同じ薄字で描くが、一覧とは行き先が違う。GUI の入力メソッドでは
    /// 一覧を候補窓へ回す一方、こちらは入力中の表示に混ぜないと意味を成さない
    /// (打っている場所の続きとして見えて初めて読める)。
    Completion,
    /// モードの印。色でモードを表すので、モードごとに分かれている。
    ModeHiragana,
    ModeKatakana,
    ModeHankaku,
    ModeZenkaku,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub style: Style,
    pub text: String,
}

/// カーソルのそばに敷く色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tint {
    pub style: Style,
    /// カーソルからの桁のずれ
    pub offset: usize,
    /// 敷く文字。`None` なら控えの文字をそのまま使う (下の文字を隠さない)。
    pub glyph: Option<char>,
}

/// 選択中の候補と、その並び。
///
/// 候補窓を自前で描く側のための形。頁分けはしていない — 端末は `inline_until` と
/// `select_keys` の数から一行に組み、GUI は窓の大きさに合わせて好きに切れる。
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateView {
    /// 候補の全体。学習で前に出たものはその順のまま。
    pub items: Vec<CandidateItem>,
    /// `items` の中で選ばれているものの位置。
    pub selected: usize,
    /// 一覧から選ぶキー。この数がそのまま一頁の大きさになる (設定の `keys.select`)。
    pub select_keys: Vec<char>,
    /// 何番目の候補から一覧を出すか (設定の `candidates.inline`)。
    ///
    /// SKK では最初の数件を一つずつ送り、それを過ぎたら一覧に切り替えるのが習わし。
    /// 常に一覧を出す GUI では読み飛ばしてよい。
    pub inline_until: usize,
}

/// 候補ひとつ。
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateItem {
    /// 画面に出す形。数値変換は展開済み、接頭辞・接尾辞の `>` は落としてある。
    pub text: String,
    /// 辞書の注釈 (`;` 以降)。
    pub annotation: Option<String>,
}

/// 重ね描きするもの。
///
/// 候補一覧を浮かせる設定では、一覧だけがカーソルから離れた行に出る。
/// 描く場所が違うので受け渡しの段階で分けておく。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preedit {
    /// カーソル位置から描く
    pub at_cursor: Vec<Segment>,
    /// 別の行に一行で浮かせる (空なら無し)
    pub floating: Vec<Segment>,
    /// セルに敷く色。
    pub cursor_tint: Option<Tint>,
}

impl Preedit {
    pub fn is_empty(&self) -> bool {
        self.at_cursor.is_empty() && self.floating.is_empty() && self.cursor_tint.is_none()
    }
}

/// キーを一つ処理した結果。
///
/// **確定した文字列と、素通しするキーは別物**として持つ。端末ではどちらも子プロセスの
/// 標準入力という同じ穴へ流すので [`Response::to_child`] で一本に組めるが、GUI の
/// 入力メソッドでは前者が「文字列の確定」、後者が「このキーは使わなかった」という
/// まったく別の知らせになる。一本のバイト列にしてしまうと、受け取った側で二度と
/// 分けられない (境目を知るには端末の作法が要り、それはこの層の持ち物ではない)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    /// 確定した文字列。
    pub commit: String,
    /// 解釈しなかったキー。呼ぶ側が自分で扱う。
    ///
    /// 確定と同時に起きることがある — ▽ の途中で矢印を押すと、見出し語を確定した
    /// うえで矢印を渡す。
    pub passthrough: Option<Key>,
    /// 入力モードが変わったか (カーソル色の更新に使う)。
    pub mode_changed: bool,
    /// 確定したあと、カーソルを何文字ぶん左へ戻すか (`$0` の位置)。
    ///
    /// 子アプリの行編集に任せる (左矢印を送る) ので、**行をまたぐ戻しはしない**。
    /// 日本語を打つ場面は必ず行編集の効くところなので同じ行なら通じるが、上の行へ
    /// 移ると編集器ごとに振る舞いが違う。またぐ場合は末尾に丸める。
    pub cursor_back: usize,
    /// 定型文の編集を求める。中身は見出し語 (決まっていなければ空)。
    ///
    /// この層は編集器を起こせない (擬似端末も画面も持たない) ので、呼ぶ側へ頼む。
    /// 端末側は画面を退避して `$EDITOR` を起こし、GUI の入力メソッドは自分の作法で
    /// 開く。戻ってきたら定型文を読み直す。
    pub edit_snippet: Option<String>,
}

impl Response {
    fn text(s: &str) -> Self {
        Response {
            commit: s.to_string(),
            ..Default::default()
        }
    }

    /// 端末へ流すバイト列。確定した文字列に、素通しするキーを続けたもの。
    pub fn to_child(&self) -> Vec<u8> {
        let mut out = self.commit.as_bytes().to_vec();
        if let Some(k) = &self.passthrough {
            out.extend(raw_bytes(k));
        }
        out
    }

    /// カーソルの戻し幅を添える。
    fn with_cursor_back(mut self, back: usize) -> Self {
        self.cursor_back = back;
        self
    }

    /// 子へ出るものが何も無いか。
    pub fn is_empty(&self) -> bool {
        self.commit.is_empty() && self.passthrough.is_none()
    }
}

/// 辞書登録の途中。候補が見つからないときに積む。
///
/// 入れ子にできる。登録内容を打っている最中にさらに未知語を変換した場合、
/// もう一段積まれる。
struct Registration {
    /// 辞書に登録する見出し語 (送りありなら `うごk`)
    key: String,
    /// 打ち込み中の登録内容
    buffer: String,
    /// 取り消したときに ▽ へ戻すための控え
    reading: String,
    okuri_head: Option<char>,
    okuri_kana: String,
    abbrev: bool,
    /// 自動変換の引き金になった文字 (登録を終えたら後ろに付ける)
    auto_suffix: String,
    /// 「定型文にしますか」を出しているか。
    ///
    /// 何も打っていないところで変換キーを押すと出る。**この位置の変換キーは
    /// もともと半角空白を溜めるだけ**で、登録内容の先頭に空白を置くことはまず
    /// 無いので、ここを譲ってもらっている。覚えるキーが増えないのが取り柄。
    offering_snippet: bool,
}

impl Registration {
    /// 画面に出す見出し (送りありは `うご*く`)。
    fn label(&self) -> String {
        match self.okuri_head {
            Some(_) => format!("{}*{}", self.reading, self.okuri_kana),
            None => self.reading.clone(),
        }
    }
}

/// 定型文の埋める場所を順に埋めている途中。
///
/// 登録の途中 ([`Registration`]) と同じ作りで、**打った文字はここに溜まる**。
/// 溜めている間も変換は使えるので、日本語をそのまま埋められる。子アプリの
/// カーソルを動かして回るのではなく、組み上がってから一度に渡す — 途中の姿は
/// 子に見えないので、shell でも編集器でも同じように動く。
struct Filling {
    /// 元の姿 (そのまま出すところと埋めるところ)
    pieces: Vec<snippet::Piece>,
    /// 埋める順の番号。`$0` は最後のカーソル位置なので入らない。
    order: Vec<u32>,
    /// いま何番目を埋めているか (`order` の添字)
    at: usize,
    /// 番号ごとに埋まった値。同じ番号が二つ以上あれば、どちらにも同じ値が入る。
    values: std::collections::HashMap<u32, String>,
}

impl Filling {
    /// 候補の本文から作る。埋める場所が無ければ `None`。
    fn new(body: &str) -> Option<Self> {
        let pieces = snippet::split_placeholders(body);
        let mut order: Vec<u32> = Vec::new();
        let mut values = std::collections::HashMap::new();
        for p in &pieces {
            if let snippet::Piece::Stop {
                index,
                default,
                choices,
            } = p
            {
                // 0 は「埋め終わったあとのカーソル位置」なので、埋める番号に入れない
                if *index != 0 && !order.contains(index) {
                    order.push(*index);
                }
                let seed = if choices.is_empty() {
                    default.clone()
                } else {
                    choices[0].clone()
                };
                values.entry(*index).or_insert(seed);
            }
        }
        if order.is_empty() {
            return None;
        }
        order.sort_unstable();
        Some(Filling {
            pieces,
            order,
            at: 0,
            values,
        })
    }

    /// いま埋めている番号。
    fn current(&self) -> u32 {
        self.order[self.at.min(self.order.len() - 1)]
    }

    /// いま埋めている場所の選択肢 (無ければ空)。
    fn choices(&self) -> Vec<String> {
        let now = self.current();
        self.pieces
            .iter()
            .find_map(|p| match p {
                snippet::Piece::Stop { index, choices, .. } if *index == now => {
                    Some(choices.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// いま埋めている値。
    fn value(&self) -> &str {
        self.values.get(&self.current()).map_or("", |s| s.as_str())
    }

    fn value_mut(&mut self) -> &mut String {
        let now = self.current();
        self.values.entry(now).or_default()
    }

    /// 組み上げた文字列と、末尾から数えたカーソルの戻し幅 (`$0` の位置)。
    ///
    /// 戻すのは子アプリに左矢印を送ってもらう形なので、**`$0` から先に改行が
    /// あるなら戻さない**。上の行へ移る動きは編集器ごとに違い、当てにできない。
    fn build(&self) -> (String, usize) {
        let mut out = String::new();
        let mut zero_at = None;
        for p in &self.pieces {
            match p {
                snippet::Piece::Text(t) => out.push_str(t),
                snippet::Piece::Stop { index: 0, .. } => zero_at = Some(out.chars().count()),
                snippet::Piece::Stop { index, .. } => {
                    out.push_str(self.values.get(index).map_or("", |s| s.as_str()))
                }
            }
        }
        let back = match zero_at {
            Some(at) => {
                let tail: String = out.chars().skip(at).collect();
                if tail.contains('\n') {
                    0
                } else {
                    tail.chars().count()
                }
            }
            None => 0,
        };
        (out, back)
    }

    /// 組み上がりつつある姿。重ね描きは一行なので、改行は印に置き換えて縮める。
    ///
    /// 打ちかけのもの (`▽たなか` や `▼田中`) は、いま埋めている場所へ差し込んで
    /// 見せる。**打っている中身が全体のどこに入るのかが、その場で分かる。**
    fn preview_with(&self, pending: &str) -> String {
        let now = self.current();
        let mut text = String::new();
        for p in &self.pieces {
            match p {
                snippet::Piece::Text(t) => text.push_str(t),
                snippet::Piece::Stop { index: 0, .. } => {}
                snippet::Piece::Stop { index, .. } => {
                    text.push_str(self.values.get(index).map_or("", |s| s.as_str()));
                    if *index == now {
                        text.push_str(pending);
                    }
                }
            }
        }
        let one_line = text.replace('\n', "⏎");
        // 長すぎると行からはみ出す。いま埋めているところを真ん中に置いて切る。
        const MAX: usize = 40;
        let chars: Vec<char> = one_line.chars().collect();
        if chars.len() <= MAX {
            return one_line;
        }
        let head: String = chars[..MAX - 1].iter().collect();
        format!("{head}…")
    }
}

/// 選べる候補ひとつ。学習と削除の宛先を候補ごとに覚えておく。
///
/// 数値変換では「だい5かい」を `だい#かい` として引くので、辞書に書き戻す先が
/// 打った見出し語と食い違う。候補の本文も `第#1回` のままで持ち、画面に出すとき
/// と子へ送るときだけ数字を戻す。こうしないと `だい#かい /第５回/` のように、
/// その数字専用の項目を辞書へ書いてしまう。
struct Choice {
    /// 学習・削除の宛先
    key: String,
    cand: Candidate,
}

/// TAB 補完の途中。見出し語を書き換えてしまうので、元に戻せるようにしておく。
struct Completion {
    /// 補完を始めたときの見出し語
    original: String,
    words: Vec<String>,
    index: usize,
}

pub struct Skk {
    pub mode: Mode,
    phase: Phase,
    romaji: Romaji,
    /// 見出し語。常にひらがなで保持し、表示時にモードへ合わせる。
    reading: String,
    /// 見出し語の中の書き込み位置 (`reading` のバイト位置)。
    ///
    /// 打っている間は常に末尾にあり、移動キーで動かしたときだけ途中を指す。
    /// 送り仮名はこの位置の外側にあるので、動かせるのは見出し語の中だけ。
    read_cursor: usize,
    /// 送り仮名のローマ字頭文字。送りあり変換の見出し語に使う。
    okuri_head: Option<char>,
    okuri_kana: String,
    /// `/` で始めた ASCII 見出し語の入力中か。
    abbrev: bool,
    /// 前置キー (sticky shift) を受け取って、次の一打鍵を待っている状態。
    ///
    /// 立っている間は打ちかけと同じ扱いにする ([`Skk::is_idle`])。ここで ASCII へ
    /// 降ろされると、次の打鍵が大文字のまま子アプリへ抜けてしまうため。
    sticky: bool,
    candidates: Vec<Choice>,
    cand_index: usize,
    /// 見出し語から取り出した数字 (数値変換で `#` に戻す)
    numbers: Vec<String>,
    /// 変換に使った見出し語 (学習の登録先)。
    dict_key: String,
    dict: Dict,
    cfg: Config,
    /// 辞書登録の積み上げ。空でなければ登録中。
    regs: Vec<Registration>,
    /// TAB 補完の途中。見出し語が変わったら捨てる。
    completion: Option<Completion>,
    /// 打つそばから見せる前方一致の見出し語 ([`Skk::refresh_suggestion`])。
    ///
    /// 打鍵のたびに引き直すので、古いものが残ることはない。無効なら常に空。
    suggestion: Vec<String>,
    /// 直前に確定した変換 (見出し語, 出力)。合成語の学習に使う。
    ///
    /// 直接入力で何か文字を出したら捨てる。接頭辞と次の語が画面上で隣り合って
    /// いることが繋げる条件なので (ddskk は `looking-at` で同じことを確かめる)。
    last_commit: Option<(String, String)>,
    /// 画面に見えている文章。同音異義語の順序を決めるのに使う。
    ///
    /// **エンジンは画面を知らない。** 端末の側が控えから組んで渡す。GUI の入力
    /// メソッドからは周辺テキストを同じ口に流せばよい。渡されなければ何もしない。
    context: Option<Context>,
    /// 直前の変換で文脈がどう効いたか。記録に出すためだけに持つ。
    ///
    /// **点数が見えないと、狙いと違う候補が出た理由を追えない。** 画面のどの語が
    /// 効いたのかは、点数を並べて初めて分かる。
    context_note: Option<String>,
    /// いまの日時 ([`Skk::set_now`])。定型文の変数を開くのに使う。
    now: snippet::Now,
    /// 定型文の埋める場所を埋めている途中 ([`Filling`])。
    filling: Option<Filling>,
    /// 埋め終わったあとに後ろへ付けるもの (送り仮名・自動変換の引き金)。
    fill_suffix: String,
    /// 自動変換 (auto-start-henkan) の引き金になった文字。
    ///
    /// 「ほんやくを」と打つと `を` の手前までで変換を始め、`を` はそのまま候補の
    /// 後ろに置く。確定するまで候補と一緒に動くので、確定した文字列の一部として
    /// 持っておく必要がある。
    auto_suffix: String,
}

impl Skk {
    pub fn new(dict: Dict, cfg: Config) -> Self {
        let mut romaji = Romaji::new();
        romaji.set_kutouten(cfg.kutouten);
        romaji.set_azik(cfg.azik);
        Skk {
            cfg,
            mode: Mode::Ascii,
            phase: Phase::Direct,
            romaji,
            reading: String::new(),
            read_cursor: 0,
            okuri_head: None,
            okuri_kana: String::new(),
            abbrev: false,
            sticky: false,
            candidates: Vec::new(),
            cand_index: 0,
            numbers: Vec::new(),
            dict_key: String::new(),
            dict,
            regs: Vec::new(),
            completion: None,
            suggestion: Vec::new(),
            last_commit: None,
            context: None,
            context_note: None,
            now: snippet::Now::default(),
            filling: None,
            fill_suffix: String::new(),
            auto_suffix: String::new(),
        }
    }

    /// モードの印の出し方 (カーソルの見た目を決めるのに使う)。
    pub fn marker(&self) -> Marker {
        self.cfg.mode_marker
    }

    pub fn dict_mut(&mut self) -> &mut Dict {
        &mut self.dict
    }

    /// 設定ファイルが書き換わったときに差し替える。
    ///
    /// 変換の途中で入れ替わっても状態は壊れない。見出し語や候補は設定に依らず、
    /// 参照するのは次のキーを解釈するときだけだから。一覧の頁の大きさが変わると
    /// 表示中の頁の切れ目は変わるが、選んでいる候補そのものはずれない。
    pub fn set_config(&mut self, cfg: Config) {
        self.romaji.set_kutouten(cfg.kutouten);
        self.romaji.set_azik(cfg.azik);
        self.cfg = cfg;
    }

    /// いまの日時を教える。定型文の `$CURRENT_YEAR` などを開くのに使う。
    ///
    /// **この層は時計を持たない** — 地方時に直すには libc が要り、端末にも GUI にも
    /// 載せられるよう libc を持たない作りにしてある。教えなければ変数は開かず、
    /// 書いたままの姿で出る。打鍵のたびに教えてよい (日付の変わり目をまたいでも
    /// 正しく出る)。
    pub fn set_now(&mut self, now: snippet::Now) {
        self.now = now;
    }

    /// いまの設定。キーの割り当てを見たい側 (入力の切り出し) に渡す。
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 打鍵によらず ASCII モードへ降ろす。動かしたときだけ true。
    ///
    /// 子アプリが「入力を受け付ける段ではなくなった」と示したときに使う。vim や
    /// nvim が挿入モードを抜けたときが典型で、**抜け方 (Esc・割り当て・コマンド) に
    /// 依らず**降ろせるのが [`Config::ascii_keys`] との違い。
    pub fn leave_to_ascii(&mut self) -> bool {
        if self.mode == Mode::Ascii || !self.is_idle() {
            return false;
        }
        self.mode = Mode::Ascii;
        true
    }

    /// 入力中の内容を確定して返す。
    ///
    /// **解釈しないキーが来たとき**に、その手前までを確定させるために使う。端末では
    /// [`Key::Raw`] を渡せば同じことが起きる (見出し語を確定してから素通しする) が、
    /// GUI の入力メソッドでは矢印や機能キーをバイト列で表せないので、確定だけを
    /// 頼む口が要る。
    ///
    /// 辞書登録の途中では何もしない — 打ち込み中の内容が消えると困る。
    pub fn flush(&mut self) -> String {
        if !self.regs.is_empty() {
            return String::new();
        }
        let text = match self.phase {
            Phase::Direct => {
                let out = self.romaji.flush();
                self.shape(&out)
            }
            Phase::Composing => {
                let text = self.confirm_reading();
                self.reset();
                text
            }
            Phase::Selecting => self.commit_candidate(),
        };
        if !text.is_empty() {
            self.last_commit = None;
        }
        text
    }

    /// 入力中の内容をすべて捨てる。モードは変えない。
    ///
    /// 打ちかけのローマ字・見出し語・候補・辞書登録が消える。GUI の入力メソッドで
    /// 窓のフォーカスが移ったときのように、**入力の続きが望めなくなった**場面で使う。
    /// 端末では子プロセスがそのまま残るので、いまのところ呼ぶ場面が無い。
    pub fn clear(&mut self) {
        self.regs.clear();
        self.last_commit = None;
        self.reset();
    }

    /// いまの入力モード。
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// 入力中のものが何も無いか。
    ///
    /// 打鍵以外の合図でモードを動かしてよいかの判断にも使う ([`Skk::leave_to_ascii`])。
    /// 変換や辞書登録、定型文を埋めている途中で降ろすと、打ち込んだものが行き場を失う。
    pub fn is_idle(&self) -> bool {
        self.regs.is_empty()
            && self.phase == Phase::Direct
            && self.romaji.is_empty()
            && self.filling.is_none()
            && !self.sticky
    }

    /// 前置キー (sticky) を受け取っている間に出す印。立っていなければ空。
    ///
    /// **押した手応えが要る。** 次の一打鍵まで画面が動かないと、効いたのかどうかが
    /// 分からない。印は ddskk が実際に入れるものと同じにしてある — かなモードでは
    /// `▽` (ここから読み)、`▽` の途中では `*` (ここから送り仮名)。意味がそのまま
    /// 印になるので、記号を新しく覚えなくて済む。
    ///
    /// 送り仮名を打っている最中は出さない。`*` はもう出ているし、そこでもう一度
    /// 押しても送りの頭は動かないため。
    fn sticky_hint(&self) -> &'static str {
        if !self.sticky {
            return "";
        }
        match self.phase {
            // ▼ で押したときは、確定して次の見出し語に入る予告になる
            Phase::Direct | Phase::Selecting => "▽",
            Phase::Composing if self.okuri_head.is_none() => "*",
            Phase::Composing => "",
        }
    }

    /// 入力途中の表示。空なら重ね描きするものは無い。
    pub fn preedit(&self) -> Preedit {
        let mut segs = Vec::new();
        let mut floating = Vec::new();
        // 定型文を埋めている最中。いま何番目かと、組み上がりつつある姿を出す。
        if let Some(f) = &self.filling {
            segs.push(Segment {
                style: Style::ListItem,
                text: format!("[埋め {}/{}]", f.at + 1, f.order.len()),
            });
            segs.push(Segment {
                style: Style::Candidate,
                text: f.preview_with(&self.pending_text()),
            });
            let choices = f.choices();
            if choices.len() > 1 {
                segs.push(Segment {
                    style: Style::ListItem,
                    text: format!(" ; {} 択", choices.len()),
                });
            }
            return Preedit {
                at_cursor: segs,
                floating,
                cursor_tint: None,
            };
        }
        // 登録中は見出しと打ち込み済みの内容を前に置く。入れ子は括弧の重なりで表す。
        if let Some(reg) = self.regs.last() {
            let depth = self.regs.len();
            segs.push(Segment {
                style: Style::ListItem,
                text: format!(
                    "{}登録:{}{}",
                    "[".repeat(depth),
                    reg.label(),
                    "]".repeat(depth)
                ),
            });
            if !reg.buffer.is_empty() {
                segs.push(Segment {
                    style: Style::Reading,
                    text: reg.buffer.clone(),
                });
            }
            // 「定型文にしますか」。候補と同じ見た目にして、選ぶものだと判るようにする。
            if reg.offering_snippet {
                segs.push(Segment {
                    style: Style::Candidate,
                    text: "▼[スニペットを登録]".to_string(),
                });
                return Preedit {
                    at_cursor: segs,
                    floating,
                    cursor_tint: None,
                };
            }
        }
        match self.phase {
            Phase::Direct => {
                if !self.sticky_hint().is_empty() {
                    segs.push(Segment {
                        style: Style::Reading,
                        text: self.sticky_hint().to_string(),
                    });
                }
                if !self.romaji.is_empty() {
                    segs.push(Segment {
                        style: Style::Romaji,
                        text: self.romaji.pending().to_string(),
                    });
                }
            }
            Phase::Composing => {
                // カーソルの手前 → 打ちかけのローマ字 → カーソルの一文字 → 残り、の順。
                // カーソルを末尾から動かしていなければ最初の区間だけになり、
                // 見出し語も送り仮名も一続きに出る。
                let (before, after) = self.reading.split_at(self.read_cursor);
                let mut head = String::from("▽");
                head.push_str(&self.shown_reading(before));
                if after.is_empty() {
                    if self.okuri_head.is_some() {
                        head.push('*');
                        head.push_str(&self.okuri_kana);
                    } else {
                        head.push_str(self.sticky_hint());
                    }
                }
                segs.push(Segment {
                    style: Style::Reading,
                    text: head,
                });
                if !self.romaji.is_empty() {
                    segs.push(Segment {
                        style: Style::Romaji,
                        text: self.romaji.pending().to_string(),
                    });
                }
                if let Some(c) = after.chars().next() {
                    segs.push(Segment {
                        style: Style::ReadingCursor,
                        text: self.shown_reading(&c.to_string()),
                    });
                    let mut tail = self.shown_reading(&after[c.len_utf8()..]);
                    if self.okuri_head.is_some() {
                        tail.push('*');
                        tail.push_str(&self.okuri_kana);
                    } else {
                        tail.push_str(self.sticky_hint());
                    }
                    if !tail.is_empty() {
                        segs.push(Segment {
                            style: Style::Reading,
                            text: tail,
                        });
                    }
                }
                segs.extend(self.completion_hint());
                self.show_suggestion(&mut segs, &mut floating);
            }
            Phase::Selecting => {
                let cur = self
                    .current_candidate()
                    .map(|c| self.shown(c))
                    .unwrap_or_default();
                segs.push(Segment {
                    style: Style::Candidate,
                    text: format!("▼{}{}{}", cur, self.okuri_kana, self.auto_suffix),
                });
                if !self.sticky_hint().is_empty() {
                    segs.push(Segment {
                        style: Style::Reading,
                        text: self.sticky_hint().to_string(),
                    });
                }
                if let Some(annot) = self
                    .current_candidate()
                    .and_then(|c| c.cand.annotation.clone())
                    .filter(|_| !self.list_visible())
                {
                    segs.push(Segment {
                        style: Style::ListItem,
                        text: format!(" ; {}", annot),
                    });
                }
                if self.list_visible() {
                    let list = &mut match self.cfg.layout {
                        Layout::Inline => &mut segs,
                        Layout::Float => &mut floating,
                    };
                    if self.cfg.layout == Layout::Inline {
                        list.push(Segment {
                            style: Style::ListItem,
                            text: " ".into(),
                        });
                    }
                    let (start, end) = self.page_range();
                    for (n, i) in (start..end).enumerate() {
                        let style = if i == self.cand_index {
                            Style::ListSelected
                        } else {
                            Style::ListItem
                        };
                        list.push(Segment {
                            style,
                            text: format!(
                                "{}:{}",
                                self.cfg.select[n],
                                self.shown(&self.candidates[i])
                            ),
                        });
                        if i + 1 < end {
                            list.push(Segment {
                                style: Style::ListItem,
                                text: " ".into(),
                            });
                        }
                    }
                    // 残りの件数を添える (ddskk と同じ)
                    let rest = self.candidates.len() - end;
                    if rest > 0 {
                        list.push(Segment {
                            style: Style::ListItem,
                            text: format!("  [残り {rest}]"),
                        });
                    }
                }
            }
        }
        let mode_style = match self.mode {
            Mode::Ascii => None,
            Mode::Hiragana => Some(Style::ModeHiragana),
            Mode::Katakana => Some(Style::ModeKatakana),
            Mode::HankakuKatakana => Some(Style::ModeHankaku),
            Mode::ZenkakuAscii => Some(Style::ModeZenkaku),
        };
        let mut cursor_tint = None;
        match self.cfg.mode_marker {
            Marker::Off => {}
            // カーソル位置のセルに色を敷く。文字を足さないので邪魔にならない。
            // 打ち込み中の文字がある間はその先頭が同じ場所に来るので出さない。
            Marker::Cell | Marker::Symbol | Marker::Beside => {
                // 打ち込み中の文字がある間はその先頭が同じ場所に来るので出さない
                if segs.is_empty() {
                    let offset = usize::from(self.cfg.mode_marker == Marker::Beside);
                    // 記号を出す方式では、色に頼らずモードが分かるようにする
                    let glyph =
                        (self.cfg.mode_marker == Marker::Symbol).then_some(match self.mode {
                            Mode::Hiragana => self.cfg.mode_symbols[0],
                            Mode::Katakana => self.cfg.mode_symbols[1],
                            Mode::HankakuKatakana => self.cfg.mode_symbols[2],
                            _ => self.cfg.mode_symbols[3],
                        });
                    cursor_tint = mode_style.map(|style| Tint {
                        style,
                        offset,
                        glyph,
                    });
                }
            }
            // 印を末尾に置く。カーソルは重ね描きの先頭に戻るので、
            // 印は打ち込み中の文字より後ろに出る。
            Marker::Letter => {
                if let Some(style) = mode_style {
                    let text = match self.mode {
                        Mode::Hiragana => "あ",
                        Mode::Katakana => "ア",
                        Mode::HankakuKatakana => "半",
                        _ => "Ａ",
                    };
                    segs.push(Segment {
                        style,
                        text: text.to_string(),
                    });
                }
            }
        }

        Preedit {
            at_cursor: segs,
            floating,
            cursor_tint,
        }
    }

    /// 見出し語を画面に出す形にする。`/` で始めた ASCII の見出し語はそのまま。
    fn shown_reading(&self, kana: &str) -> String {
        if self.abbrev {
            return kana.to_string();
        }
        self.shape(kana)
    }

    /// いま打ちかけのものを一続きの文字にする (定型文を埋めている最中の表示用)。
    fn pending_text(&self) -> String {
        match self.phase {
            Phase::Direct => format!("{}{}", self.sticky_hint(), self.romaji.pending()),
            Phase::Composing => format!(
                "▽{}{}{}",
                self.shown_reading(&self.reading),
                self.romaji.pending(),
                self.sticky_hint()
            ),
            Phase::Selecting => format!(
                "▼{}{}",
                self.current_candidate()
                    .map(|c| self.shown(c))
                    .unwrap_or_default(),
                self.sticky_hint()
            ),
        }
    }

    fn current_candidate(&self) -> Option<&Choice> {
        self.candidates.get(self.cand_index)
    }

    /// 選択中 (▼) の候補を、そのまま扱える形で返す。▼ でなければ `None`。
    ///
    /// [`Skk::preedit`] は端末に重ね描きするために候補を一行へ組み上げてしまうので、
    /// 候補窓を自前で描く側 (GUI の入力メソッドなど) はこちらを使う。頁の切り方は
    /// 呼ぶ側に任せ、ここでは全体と選択位置だけを渡す。
    pub fn candidates(&self) -> Option<CandidateView> {
        if self.phase != Phase::Selecting {
            return None;
        }
        Some(CandidateView {
            items: self
                .candidates
                .iter()
                .map(|c| CandidateItem {
                    text: self.shown(c),
                    annotation: c.cand.annotation.clone(),
                })
                .collect(),
            selected: self.cand_index,
            select_keys: self.cfg.select.clone(),
            inline_until: self.cfg.inline_candidates,
        })
    }

    /// 候補の本文を、画面と出力に出す形にする。
    ///
    /// 数字を戻し、接頭辞・接尾辞の印を落とす。`SKK-JISYO.L` では候補側に `>` が
    /// 付くのは 1 件だけだが、skkeleton も同じ処理を持つ。
    fn shown(&self, c: &Choice) -> String {
        let t = num::expand(&c.cand.text, &self.numbers);
        // 定型文に書いた日付や時刻を、いまの値に開く。知らない名前は残るので、
        // `$100` のような普通の候補は素通りする。
        let t = snippet::expand_variables(&t, &self.now);
        // 定型文の埋める場所は、既定値を入れた姿で見せる。`${1:宛先}` という
        // 書き方そのものを見せても、選ぶ手掛かりにならない。
        let t = if self.dict.is_snippet(&c.key, &c.cand.text) {
            snippet::preview_placeholders(&t)
        } else {
            t
        };
        if c.key.ends_with('>') {
            t.trim_end_matches('>').to_string()
        } else if c.key.starts_with('>') {
            t.trim_start_matches('>').to_string()
        } else {
            t
        }
    }

    /// 確定を記録し、接頭辞・接尾辞に続いた場合は繋げて覚える。
    ///
    /// ddskk の `skk-learn-combined-word` と同じ。「さい>」→再 のあと
    /// 「りよう」→利用 と確定したら `さいりよう /再利用/` を覚える。
    /// 送りありの語は繋げない (候補が語幹だけなので繋いでも筋が通らない)。
    fn note_commit(&mut self, key: String, text: String, okuri: bool) {
        let prev = self.last_commit.take();
        if self.cfg.learn_combined
            && !okuri
            && let Some((pk, pt)) = prev
        {
            let joined = if pk.ends_with('>') && !key.ends_with('>') && !key.starts_with('>') {
                // 接頭辞のあとに普通の語
                Some((
                    format!("{}{}", &pk[..pk.len() - 1], key),
                    format!("{pt}{text}"),
                ))
            } else if key.starts_with('>') && !pk.starts_with('>') && !pk.ends_with('>') {
                // 普通の語のあとに接尾辞
                Some((format!("{}{}", pk, &key[1..]), format!("{pt}{text}")))
            } else {
                None
            };
            if let Some((k, joined_text)) = joined
                && !joined_text.is_empty()
            {
                self.dict.learn(
                    &k,
                    &Candidate {
                        text: joined_text,
                        annotation: None,
                    },
                );
            }
        }
        if okuri {
            self.last_commit = None;
        } else {
            self.last_commit = Some((key, text));
        }
    }

    /// 補完中の見出し語に添えるもの。補完していなければ空。
    ///
    /// **見出し語だけでは選べない。** 前方一致で伸ばした「かんじゃ」が患者なのか
    /// 冠者なのかは、変換するまで分からない。TAB を送るたびに一度 space を押して
    /// 確かめ、違えば戻る、という往復になっていた。
    ///
    /// 出すのは三つ。space を押したら何になるか (第一候補)、その注釈、そして
    /// **いま何番目か** — 一周したのか、まだ先があるのかが分かる。
    fn completion_hint(&self) -> Vec<Segment> {
        let Some(c) = self.completion.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(cand) = self.dict.lookup(&self.reading).into_iter().next() {
            out.push(Segment {
                style: Style::ListItem,
                text: format!(" {}", cand.text),
            });
            if let Some(annot) = &cand.annotation {
                out.push(Segment {
                    style: Style::ListItem,
                    text: format!(" ; {annot}"),
                });
            }
        }
        out.push(Segment {
            style: Style::ListItem,
            text: format!(" [{}/{}]", c.index + 1, c.words.len()),
        });
        out
    }

    fn list_visible(&self) -> bool {
        self.cand_index >= self.cfg.inline_candidates
    }

    fn page_range(&self) -> (usize, usize) {
        let inline = self.cfg.inline_candidates;
        let size = self.cfg.page_size();
        let page = (self.cand_index - inline) / size;
        let start = inline + page * size;
        (start, (start + size).min(self.candidates.len()))
    }

    fn reset(&mut self) {
        self.completion = None;
        self.phase = Phase::Direct;
        self.romaji.clear();
        self.reading.clear();
        self.read_cursor = 0;
        self.okuri_head = None;
        self.okuri_kana.clear();
        self.abbrev = false;
        self.candidates.clear();
        self.cand_index = 0;
        self.numbers.clear();
        self.dict_key.clear();
        self.auto_suffix.clear();
    }

    // ---- 見出し語の書き込み位置 ----
    //
    // 見出し語は打った順に伸びるだけなので、普段カーソルは末尾にいる。移動キーで
    // 途中へ動かしたときだけ、差し込みと削除の位置が末尾から離れる。送り仮名は
    // 見出し語の外側にあり、この位置の対象にはならない。

    /// 見出し語のカーソル位置へ差し込み、カーソルをその後ろへ送る。
    fn insert_reading(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.reading.insert_str(self.read_cursor, s);
        self.read_cursor += s.len();
    }

    /// 見出し語を丸ごと置き換える。カーソルは末尾へ。
    fn set_reading(&mut self, s: String) {
        self.reading = s;
        self.read_cursor = self.reading.len();
    }

    /// カーソルの手前の一文字を消す。消せたら true。
    fn delete_before_cursor(&mut self) -> bool {
        let Some(c) = self.reading[..self.read_cursor].chars().next_back() else {
            return false;
        };
        self.read_cursor -= c.len_utf8();
        self.reading.remove(self.read_cursor);
        true
    }

    /// カーソル位置の一文字を消す。消せたら true。
    fn delete_at_cursor(&mut self) -> bool {
        if self.read_cursor >= self.reading.len() {
            return false;
        }
        self.reading.remove(self.read_cursor);
        true
    }

    /// カーソルを一文字ずらす。`dir` は -1 で左、1 で右。端では止まる。
    fn move_cursor(&mut self, dir: i32) {
        if dir < 0 {
            if let Some(c) = self.reading[..self.read_cursor].chars().next_back() {
                self.read_cursor -= c.len_utf8();
            }
        } else if let Some(c) = self.reading[self.read_cursor..].chars().next() {
            self.read_cursor += c.len_utf8();
        }
    }

    /// カーソルを見出し語の途中へ動かしてあるか。
    fn cursor_inside_reading(&self) -> bool {
        self.read_cursor < self.reading.len()
    }

    /// 打ちかけのローマ字を、いま打ち込んでいる先 (送り仮名か見出し語) へ落とす。
    ///
    /// [`Romaji::flush`] は `n` だけを「ん」にして、他の打ちかけの子音は捨てる。
    /// 確定するときとまったく同じ扱いなので、見出し語にローマ字が紛れ込まない。
    fn flush_pending(&mut self) {
        let flushed = self.romaji.flush();
        if flushed.is_empty() {
            return;
        }
        if self.okuri_head.is_some() {
            self.okuri_kana.push_str(&flushed);
        } else {
            self.insert_reading(&flushed);
        }
    }

    /// かなをモードに合わせて整える。
    fn shape(&self, kana: &str) -> String {
        match self.mode {
            Mode::Katakana => romaji::to_katakana(kana),
            Mode::HankakuKatakana => romaji::to_hankaku_katakana(kana),
            _ => kana.to_string(),
        }
    }

    pub fn handle(&mut self, key: Key) -> Response {
        let r = self.handle_key(key);
        // 動的補完は「いまの見出し語」に紐づくので、状態が動いたあとに引き直す。
        // 入り口を一つに絞ってあるので、どの経路を通っても取り残されない。
        self.refresh_suggestion();
        r
    }

    fn handle_key(&mut self, key: Key) -> Response {
        // 定型文の編集はどの段からでも呼べる。割り当てが無ければ何も起きない。
        // ASCII モードでは効かせない — 子アプリの持ち物であるキーを奪ってしまう。
        if self.mode != Mode::Ascii
            && !self.cfg.snippet_edit.is_empty()
            && self.cfg.snippet_edit.contains(&key)
        {
            return Response {
                edit_snippet: Some(self.word_in_hand()),
                ..Default::default()
            };
        }
        // 定型文の埋める場所を埋めている最中。打った文字はそこへ溜まる。
        if self.filling.is_some() {
            return self.handle_filling(key);
        }
        if self.regs.is_empty() {
            // 挿入モードを抜けたときにかなが残らないようにする。vim / nvim で
            // Esc を押すと挿入モードを抜けるので、同じキーで ASCII へ戻す。
            // 登録の途中では効かせない (打ち込みの最中に消えると困る)。
            if self.mode != Mode::Ascii && self.cfg.ascii_keys.contains(&key) {
                let mut r = self.dispatch(key);
                self.mode = Mode::Ascii;
                r.mode_changed = true;
                return r;
            }
            return self.dispatch(key);
        }
        // 登録中。子へ出るはずだった文字は登録内容に溜める。
        if matches!(key, Key::Raw(_)) {
            // 矢印などは受け付けない。挟むと登録内容が壊れる。
            return Response::default();
        }
        if let Key::Paste(text) = &key {
            // 貼り付けた中身はそのまま登録内容にする (変換も囲みの列も挟まない)
            if let Ok(s) = std::str::from_utf8(text) {
                let reg = self.regs.last_mut().expect("登録中");
                reg.buffer.extend(s.chars().filter(|c| !c.is_control()));
            }
            return Response::default();
        }
        // 「定型文にしますか」を出している間は、決めるか引っ込めるかの二択。
        if self.regs.last().is_some_and(|r| r.offering_snippet) {
            return self.answer_snippet_offer(key);
        }
        if self.phase == Phase::Direct {
            if key == Key::Enter {
                return self.finish_registration();
            }
            if self.romaji.is_empty() {
                if self.cfg.cancel.contains(&key) {
                    return self.abort_registration();
                }
                if self.cfg.backspace.contains(&key) {
                    let reg = self.regs.last_mut().expect("登録中");
                    if reg.buffer.pop().is_none() {
                        return self.abort_registration();
                    }
                    return Response::default();
                }
                // 何も打っていないところでの変換キー。もともと半角空白を溜めるだけ
                // なので、ここで「定型文にしますか」を出す。
                if self.cfg.convert.contains(&key)
                    && self.regs.last().is_some_and(|r| r.buffer.is_empty())
                {
                    self.regs.last_mut().expect("登録中").offering_snippet = true;
                    return Response::default();
                }
            }
        }
        let r = self.dispatch(key);
        self.capture(r)
    }

    /// いま手にしている見出し語。定型文を書きに行くときに持って行く。
    ///
    /// 登録の途中ならその見出し語、▽ や ▼ ならいま打っているもの。何も打って
    /// いなければ空 (編集器の側で決める)。
    fn word_in_hand(&self) -> String {
        if let Some(reg) = self.regs.last() {
            return reg.reading.clone();
        }
        match self.phase {
            Phase::Composing | Phase::Selecting => self.reading.clone(),
            Phase::Direct => String::new(),
        }
    }

    /// 「定型文にしますか」への返事。
    ///
    /// 決める (確定キー / Enter) と、見出し語を添えて呼ぶ側へ頼む。取り消す
    /// (`C-g`) と引っ込めて登録の続きへ戻る。**それ以外のキーも引っ込めてから
    /// 改めて処理する** — 打ち始めた人を止めない。
    fn answer_snippet_offer(&mut self, key: Key) -> Response {
        let reg = self.regs.last_mut().expect("登録中");
        reg.offering_snippet = false;

        if key == Key::Enter || self.cfg.confirm.contains(&key) {
            let key_word = reg.reading.clone();
            // 登録そのものは畳む。定型文として書くので、辞書には入れない。
            let reg = self.regs.pop().expect("登録中");
            let mut r = self.resume_composing(reg);
            r.edit_snippet = Some(key_word);
            return r;
        }
        if self.cfg.cancel.contains(&key) {
            return Response::default();
        }
        self.handle(key)
    }

    /// 子へ出るはずだった文字を、いちばん内側の登録内容へ回す。
    ///
    /// 制御文字は捨てる。割り当ての無い `C-z` などがそのまま混ざると、
    /// 辞書に制御文字を含む項目ができてしまうため。素通しするキーのうち拾うのは
    /// 文字だけ (ASCII モードで打った英字など)。矢印のようなキーは捨てる。
    fn capture(&mut self, r: Response) -> Response {
        let Some(reg) = self.regs.last_mut() else {
            return r;
        };
        reg.buffer
            .extend(r.commit.chars().filter(|c| !c.is_control()));
        if let Some(Key::Char(c)) = r.passthrough
            && !c.is_control()
        {
            reg.buffer.push(c);
        }
        Response {
            mode_changed: r.mode_changed,
            ..Default::default()
        }
    }

    fn dispatch(&mut self, key: Key) -> Response {
        let Some(key) = self.take_sticky(key) else {
            // 前置キーを受け取っただけ。画面は動かさず次の一打鍵を待つ。
            return Response::default();
        };
        match self.phase {
            Phase::Direct => self.handle_direct(key),
            Phase::Composing => self.handle_composing(key),
            Phase::Selecting => self.handle_selecting(key),
        }
    }

    /// 前置キー (sticky shift) を解いて、実際に処理する打鍵を返す。
    ///
    /// Shift の同時押しがしていること — 「ここから読み」「ここから送り仮名」という
    /// **区切りの宣言** — を、押す順番に置き換えたもの。宣言そのものは無くならない
    /// (無くせば読みの範囲を推し量ることになり、SKK ではなくなる) ので、同時押しと
    /// 打鍵一つを取り替えているだけ。ddskk の `skk-sticky-key` と同じ考え方。
    ///
    /// 前置キーを受け取った打鍵では何も起こさない (`None`)。続けてもう一度押せば
    /// その文字が出る (`;;` → `;`) — 記号は大文字にしても変わらないので、素通し
    /// するだけで済む。
    fn take_sticky(&mut self, key: Key) -> Option<Key> {
        if self.sticky {
            self.sticky = false;
            return Some(match key {
                Key::Char(c) => Key::Char(c.to_ascii_uppercase()),
                k => k,
            });
        }
        // かなモードでだけ効かせる。ASCII・全角英数のキーは子アプリの持ち物で、
        // `/` の ASCII 見出し語では記号をそのまま打ちたい。
        if self.mode.is_kana() && !self.abbrev && self.cfg.sticky.contains(&key) {
            self.sticky = true;
            return None;
        }
        Some(key)
    }

    /// 候補が尽きたので辞書登録を始める。
    fn begin_registration(&mut self) {
        self.regs.push(Registration {
            key: std::mem::take(&mut self.dict_key),
            buffer: String::new(),
            reading: self.reading.clone(),
            okuri_head: self.okuri_head,
            okuri_kana: self.okuri_kana.clone(),
            abbrev: self.abbrev,
            auto_suffix: std::mem::take(&mut self.auto_suffix),
            offering_snippet: false,
        });
        self.reset();
    }

    /// 登録を確定する。空のまま確定したときは登録せず ▽ へ戻す。
    fn finish_registration(&mut self) -> Response {
        let flushed = self.romaji.flush();
        let shaped = self.shape(&flushed);
        if let Some(reg) = self.regs.last_mut() {
            reg.buffer.push_str(&shaped);
        }
        let reg = self.regs.pop().expect("登録中");
        if reg.buffer.is_empty() {
            return self.resume_composing(reg);
        }
        let cand = Candidate {
            text: reg.buffer.clone(),
            annotation: None,
        };
        self.dict.learn(&reg.key, &cand);
        // 登録した語も送り仮名ごとの宛先へ入れる。次に同じ送り仮名で打ったとき、
        // いま登録したものが先に出る。
        if reg.okuri_head.is_some() {
            self.dict
                .learn_okuri(&reg.key, &reg.okuri_kana, &cand.text);
        }
        let text = format!("{}{}{}", reg.buffer, reg.okuri_kana, reg.auto_suffix);
        self.capture(Response::text(&text))
    }

    /// 登録を取り消して ▽ に戻す。打ちかけの登録内容は捨てる。
    fn abort_registration(&mut self) -> Response {
        let reg = self.regs.pop().expect("登録中");
        self.resume_composing(reg)
    }

    fn resume_composing(&mut self, reg: Registration) -> Response {
        self.reset();
        self.phase = Phase::Composing;
        self.set_reading(reg.reading);
        self.okuri_head = reg.okuri_head;
        self.okuri_kana = reg.okuri_kana;
        self.abbrev = reg.abbrev;
        Response::default()
    }

    // ---- 直接入力 ----

    fn handle_direct(&mut self, key: Key) -> Response {
        let r = self.direct_inner(key);
        // 何か出したなら、接頭辞と次の語はもう隣り合っていない
        if !r.is_empty() {
            self.last_commit = None;
        }
        r
    }

    fn direct_inner(&mut self, key: Key) -> Response {
        // ASCII / 全角英数モードはほぼ素通し
        if !self.mode.is_kana() {
            return match key {
                k if self.cfg.kana.contains(&k) => {
                    self.mode = Mode::Hiragana;
                    Response {
                        mode_changed: true,
                        ..Default::default()
                    }
                }
                Key::Char(c) if self.mode == Mode::ZenkakuAscii => {
                    Response::text(&romaji::to_zenkaku(c).to_string())
                }
                k => Response {
                    passthrough: Some(k),
                    ..Default::default()
                },
            };
        }

        match key {
            k if self.cfg.confirm.contains(&k) => {
                // 途中のローマ字を確定させる
                let out = self.romaji.flush();
                Response::text(&self.shape(&out))
            }
            k if self.cfg.cancel.contains(&k) => {
                // 途中のローマ字を捨てる
                self.romaji.clear();
                Response::default()
            }
            k if self.cfg.backspace.contains(&k) => {
                if self.romaji.backspace() {
                    Response::default()
                } else {
                    // 消すものが無ければ**押されたキーのまま**子へ回す。
                    //
                    // Backspace に揃えてはいけない。`C-h` を `0x7f` にすり替えると、
                    // `C-h` に別の働きを割り当てているアプリでそれが効かなくなる
                    // (nvim の窓の移動がそう)。文字を打つ段では、どのみちアプリ側が
                    // `C-h` を手前の一文字消しに割り当てているので、そのまま渡せば
                    // 消える — **消す意味は押した先が決めればよい。**
                    Response {
                        passthrough: Some(k),
                        ..Default::default()
                    }
                }
            }
            k if self.romaji.is_empty() && self.cfg.ascii.contains(&k) => {
                self.mode = Mode::Ascii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.zenkaku.contains(&k) => {
                self.mode = Mode::ZenkakuAscii;
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.hankaku_katakana.contains(&k) => {
                self.mode = if self.mode == Mode::HankakuKatakana {
                    Mode::Hiragana
                } else {
                    Mode::HankakuKatakana
                };
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.romaji.is_empty() && self.cfg.katakana.contains(&k) => {
                self.mode = if self.mode == Mode::Hiragana {
                    Mode::Katakana
                } else {
                    Mode::Hiragana
                };
                Response {
                    mode_changed: true,
                    ..Default::default()
                }
            }
            k if self.cfg.start_conversion.contains(&k) => {
                self.phase = Phase::Composing;
                self.romaji.clear();
                Response::default()
            }
            k if self.romaji.is_empty() && self.cfg.abbrev.contains(&k) => {
                self.phase = Phase::Composing;
                self.abbrev = true;
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() => {
                self.phase = Phase::Composing;
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                self.insert_reading(&kana);
                Response::default()
            }
            Key::Char(c) => {
                let out = self.romaji.feed(c);
                Response::text(&self.shape(&out))
            }
            k => {
                // 制御キーや矢印は素通し。途中のローマ字は先に確定させる。
                let flushed = self.romaji.flush();
                Response {
                    commit: self.shape(&flushed),
                    passthrough: Some(k),
                    mode_changed: false,
                    cursor_back: 0,
                    edit_snippet: None,
                }
            }
        }
    }

    // ---- ▽ 見出し語入力 ----

    fn handle_composing(&mut self, key: Key) -> Response {
        // 見出し語が変われば補完の並びは意味を失う。補完そのものと取り消し
        // (元へ戻す) 以外のキーでは捨てる。
        let keep = self.cfg.complete.contains(&key)
            || self.cfg.complete_previous.contains(&key)
            || self.cfg.cancel.contains(&key);
        let r = self.composing_inner(key);
        if !keep {
            self.completion = None;
        }
        r
    }

    fn composing_inner(&mut self, key: Key) -> Response {
        match key {
            k if self.cfg.cancel.contains(&k) => {
                // 補完の途中なら、まず補完を取り消して元の見出し語に戻す
                if let Some(c) = self.completion.take() {
                    self.set_reading(c.original);
                    return Response::default();
                }
                // 取り消して何も出さない
                self.reset();
                Response::default()
            }
            // Enter は割り当てに依らず確定として扱う。端末では改行が「コマンドの
            // 実行」を意味するので、変換の途中で子へ送るわけにいかない。
            Key::Enter => {
                let text = self.confirm_reading();
                self.reset();
                Response::text(&text)
            }
            k if self.cfg.confirm.contains(&k) => {
                let text = self.confirm_reading();
                self.reset();
                Response::text(&text)
            }
            // 見出し語の中を動く。打ちかけのローマ字はその場に落としてから動かす。
            k if self.cfg.move_left.contains(&k) => {
                self.flush_pending();
                self.move_cursor(-1);
                Response::default()
            }
            k if self.cfg.move_right.contains(&k) => {
                self.flush_pending();
                self.move_cursor(1);
                Response::default()
            }
            k if self.cfg.move_home.contains(&k) => {
                self.flush_pending();
                self.read_cursor = 0;
                Response::default()
            }
            k if self.cfg.move_end.contains(&k) => {
                self.flush_pending();
                self.read_cursor = self.reading.len();
                Response::default()
            }
            k if self.cfg.delete_forward.contains(&k) => {
                // カーソルの乗っている一文字。末尾では何も起きない。
                self.delete_at_cursor();
                Response::default()
            }
            k if self.cfg.backspace.contains(&k) => {
                if self.romaji.backspace() {
                } else if self.cursor_inside_reading() {
                    // 途中へ動かしてある間は、送り仮名より先に見出し語のそこを直す
                    // (先頭にいるなら何も起きない)
                    self.delete_before_cursor();
                } else if !self.okuri_kana.is_empty() {
                    self.okuri_kana.pop();
                } else if self.okuri_head.is_some() {
                    self.okuri_head = None;
                } else if !self.delete_before_cursor() {
                    self.reset();
                }
                Response::default()
            }
            k if self.cfg.affix.contains(&k)
                && !self.reading.is_empty()
                && self.okuri_head.is_none()
                && !self.abbrev =>
            {
                // 接頭辞。見出し語の末尾に `>` を足してすぐ変換する。
                self.flush_pending();
                self.reading.push('>');
                self.start_conversion();
                Response::default()
            }
            k if self.cfg.complete.contains(&k) => {
                self.step_completion(1);
                Response::default()
            }
            k if self.cfg.complete_previous.contains(&k) => {
                self.step_completion(-1);
                Response::default()
            }
            k if self.cfg.convert.contains(&k) => {
                self.completion = None;
                self.start_conversion();
                Response::default()
            }
            k if !self.abbrev && self.cfg.hankaku_katakana.contains(&k) => {
                // 見出し語を半角カタカナにして確定する
                self.flush_pending();
                let text = format!(
                    "{}{}",
                    romaji::to_hankaku_katakana(&self.reading),
                    romaji::to_hankaku_katakana(&self.okuri_kana)
                );
                self.reset();
                Response::text(&text)
            }
            k if self.okuri_head.is_none() && !self.abbrev && self.cfg.katakana.contains(&k) => {
                // 見出し語をカタカナ (カタカナモードならひらがな) にして確定
                self.flush_pending();
                let text = if self.mode == Mode::Katakana {
                    romaji::to_hiragana(&self.reading)
                } else {
                    romaji::to_katakana(&self.reading)
                };
                self.reset();
                Response::text(&text)
            }
            Key::Char(c) if self.abbrev => {
                self.insert_reading(&c.to_string());
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() && !self.reading.is_empty() => {
                // 送り仮名の始まり
                if self.okuri_head.is_none() {
                    self.okuri_head = Some(c.to_ascii_lowercase());
                }
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                if !kana.is_empty() {
                    self.okuri_kana.push_str(&kana);
                    self.start_conversion();
                }
                Response::default()
            }
            Key::Char(c) if c.is_ascii_uppercase() => {
                let kana = self.romaji.feed(c.to_ascii_lowercase());
                self.insert_reading(&kana);
                Response::default()
            }
            Key::Char(c) => {
                let kana = self.romaji.feed(c);
                if self.okuri_head.is_some() {
                    self.okuri_kana.push_str(&kana);
                    if !self.okuri_kana.is_empty() && self.romaji.is_empty() {
                        self.start_conversion();
                    }
                } else if self.auto_start(&kana) {
                    // 区切りの文字が来たので、その手前までで変換を始めた
                } else {
                    self.insert_reading(&kana);
                }
                Response::default()
            }
            k => {
                // 想定外のキーは見出し語を確定してから素通しする
                let text = self.confirm_reading();
                self.reset();
                Response {
                    commit: text,
                    passthrough: Some(k),
                    mode_changed: false,
                    cursor_back: 0,
                    edit_snippet: None,
                }
            }
        }
    }

    /// ▽ の内容をそのまま (かなのまま) 確定させた文字列。
    fn confirm_reading(&mut self) -> String {
        self.flush_pending();
        let body = self.shown_reading(&self.reading);
        format!("{}{}", body, self.okuri_kana)
    }

    /// 見出し語を前方一致で補完する。`dir` は 1 で次、-1 で前。
    ///
    /// 補完の元は利用者辞書が先、共有辞書が後ろ。実際に使った語のほうが当たり
    /// やすいため。一度作った並びは見出し語が変わるまで使い回す。
    fn step_completion(&mut self, dir: i32) {
        if self.abbrev {
            return;
        }
        if self.completion.is_none() {
            // 途中のローマ字を確定してから探す
            self.flush_pending();
            let words = self.dict.complete(&self.reading, COMPLETIONS);
            if words.is_empty() {
                return;
            }
            self.completion = Some(Completion {
                original: std::mem::take(&mut self.reading),
                words,
                index: 0,
            });
        } else if let Some(c) = self.completion.as_mut() {
            let n = c.words.len() as i32;
            c.index = ((c.index as i32 + dir).rem_euclid(n)) as usize;
        }
        if let Some(c) = self.completion.as_ref() {
            let word = c.words[c.index].clone();
            self.set_reading(word);
        }
    }

    /// 打つそばから見せる見出し語を引き直す ([`Config::dynamic_completion`])。
    ///
    /// 引くのは「▽ の末尾で、かなが一区切りついたところ」だけ。ローマ字が打ちかけの
    /// うちは伸ばす先が決まらず (`k` の続きは分からない)、送り仮名に入ったあとや
    /// 見出し語の途中へ戻ったあとは、伸ばす場所が末尾ではなくなる。`TAB` で補完して
    /// いる間も出さない — そちらは既に選んでいる最中で、[`Skk::completion_hint`] が
    /// 何番目かを示している。
    fn refresh_suggestion(&mut self) {
        self.suggestion.clear();
        let limit = match self.cfg.dynamic_completion {
            DynamicCompletion::Off => return,
            DynamicCompletion::Single => 1,
            DynamicCompletion::Multiple => DYNAMIC_SHOWN,
        };
        if self.phase != Phase::Composing
            || self.abbrev
            || self.completion.is_some()
            || self.okuri_head.is_some()
            || !self.romaji.is_empty()
            || self.reading.is_empty()
            || self.read_cursor != self.reading.len()
        {
            return;
        }
        self.suggestion = self.dict.complete(&self.reading, limit);
    }

    /// 動的補完を表示に足す。`TAB` を押せば先頭のものが見出し語に入る。
    ///
    /// **打った分と見分けが付くようにする。** 見出し語は太字 + 下線、補ったものは
    /// 薄字で、どこまでが自分の打鍵かが色で分かる。
    fn show_suggestion(&self, segs: &mut Vec<Segment>, floating: &mut Vec<Segment>) {
        let Some(first) = self.suggestion.first() else {
            return;
        };
        match self.cfg.dynamic_completion {
            DynamicCompletion::Off => {}
            DynamicCompletion::Single => {
                // 見出し語の続きだけを添える。行の長さが伸びるのは補う分だけで済む。
                segs.push(Segment {
                    style: Style::Completion,
                    text: self.shown_reading(&first[self.reading.len()..]),
                });
            }
            DynamicCompletion::Multiple => {
                // 並べ方は候補一覧に合わせる。一覧が浮くようにしてある人の画面で、
                // 補完だけが行を伸ばしては辻褄が合わない。
                let inline = self.cfg.layout == Layout::Inline;
                let list = if inline { &mut *segs } else { floating };
                for (i, w) in self.suggestion.iter().enumerate() {
                    let sep = if i == 0 && !inline { "" } else { " " };
                    list.push(Segment {
                        style: Style::Completion,
                        text: format!("{sep}{}", self.shown_reading(w)),
                    });
                }
            }
        }
    }

    /// 自動変換 (auto-start-henkan)。区切りの文字が来たら、その手前までで変換を始める。
    ///
    /// 「ほんやくを」と打つと `を` の直前までの `ほんやく` で変換に入り、`を` は
    /// 候補の後ろに置かれる (ddskk の `skk-auto-start-henkan`)。**引き金の文字は
    /// 見出し語に含めない** — 含めると辞書を引けなくなる。
    ///
    /// 始めたなら true。呼ぶ側はかなを見出し語へ足さない。
    fn auto_start(&mut self, kana: &str) -> bool {
        let Some(last) = kana.chars().next_back() else {
            return false;
        };
        if self.abbrev || !self.cfg.auto_start_henkan.contains(&last) {
            return false;
        }
        // 引き金の手前までは見出し語の一部
        let head = &kana[..kana.len() - last.len_utf8()];
        if self.reading.is_empty() && head.is_empty() {
            // 見出し語が空では引きようがない。ただの文字として扱う。
            return false;
        }
        self.insert_reading(head);
        self.auto_suffix = last.to_string();
        self.start_conversion();
        true
    }

    fn start_conversion(&mut self) {
        self.flush_pending();
        // 変換を始めたら書き込み位置は末尾へ。▼ から ▽ に戻したときも末尾から始まる。
        self.read_cursor = self.reading.len();
        if self.reading.is_empty() {
            return;
        }
        self.dict_key = match self.okuri_head {
            Some(h) => format!("{}{}", self.reading, h),
            None => self.reading.clone(),
        };
        // まず打った通りに引く。学習も削除もこの見出し語が宛先になる。
        self.candidates = self
            .dict
            .lookup(&self.dict_key)
            .into_iter()
            .map(|cand| Choice {
                key: self.dict_key.clone(),
                cand,
            })
            .collect();
        // 数字を含むなら `#` に置き換えた見出し語でも引く (だい5かい → だい#かい)
        let (abstracted, numbers) = num::abstract_numbers(&self.dict_key);
        self.numbers = numbers;
        if abstracted != self.dict_key {
            for cand in self.dict.lookup(&abstracted) {
                if !self.candidates.iter().any(|c| c.cand.text == cand.text) {
                    self.candidates.push(Choice {
                        key: abstracted.clone(),
                        cand,
                    });
                }
            }
        }
        // 文脈が先、送り仮名が後。**送り仮名は語を決める手掛かり、文脈は好みの
        // 手掛かり**なので、食い違ったら送り仮名を優先する (後の並べ替えが安定なので、
        // 送り仮名で同じ組に入ったものの中では文脈の順が残る)。
        self.sort_by_context();
        self.sort_by_okuri();
        if self.candidates.is_empty() {
            // 候補が無ければ辞書登録へ移る
            self.begin_registration();
            return;
        }
        self.cand_index = 0;
        self.phase = Phase::Selecting;
    }

    /// 直前の変換で文脈がどう効いたかを取り出す (記録用)。一度取ると消える。
    pub fn take_context_note(&mut self) -> Option<String> {
        self.context_note.take()
    }

    /// 文脈を渡す意味があるか。無効なら組み立て自体を省ける。
    pub fn wants_context(&self) -> bool {
        self.cfg.context_order
    }

    /// 画面に見えている文章を渡す。`cursor` は文字数での位置。
    ///
    /// 変換を始めるたびに見るので、画面が変わったときだけ渡し直せばよい。
    pub fn set_context(&mut self, text: &str, cursor: usize) {
        self.context = (!text.is_empty()).then(|| {
            Context::with_half_distance(text, cursor, self.cfg.context_half_distance)
        });
    }

    /// 画面の文脈で候補を並べ替える。
    ///
    /// **日本語は同音異義語が多すぎる。** 「こうせい」には構成・公正・校正・攻勢・
    /// 後世・更生・厚生・恒星… と 20 件を超える候補があり、見出し語だけでは決めよう
    /// がない。画面に見えている文章を手掛かりに寄せる。
    ///
    /// **手掛かりが無ければ並びを変えない。** 点数が全部 0 のときは触らないので、
    /// 関係のない画面で悪くなることがない。
    fn sort_by_context(&mut self) {
        if !self.cfg.context_order {
            return;
        }
        let Some(ctx) = self.context.as_ref() else {
            return;
        };
        if ctx.is_empty() || self.candidates.len() < 2 {
            return;
        }
        let explained: Vec<(f64, Vec<(String, f64)>)> = self
            .candidates
            .iter()
            .map(|c| ctx.explain(&c.cand.text, c.cand.annotation.as_deref()))
            .collect();
        let scored: Vec<f64> = explained.iter().map(|(s, _)| *s).collect();
        if scored.iter().all(|s| *s <= 0.0) {
            return;
        }
        // 点の付いたものを、**効いた語を添えて**重い順に記録へ残す
        let mut note: Vec<(f64, String)> = explained
            .iter()
            .zip(&self.candidates)
            .filter(|((s, _), _)| *s > 0.0)
            .map(|((s, hits), c)| {
                let why: Vec<String> = hits.iter().take(3).map(|(w, _)| w.clone()).collect();
                (*s, format!("{} {s:.2}[{}]", c.cand.text, why.join(",")))
            })
            .collect();
        note.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        self.context_note = Some(format!(
            "{} → {}\n    画面 {}",
            self.dict_key,
            note.iter()
                .take(5)
                .map(|(_, t)| t.clone())
                .collect::<Vec<_>>()
                .join(" / "),
            ctx.digest(60)
        ));
        // 点数の降順。**安定な並べ替え**なので、同点のものは元の順 (最近使った順) を保つ。
        let mut order: Vec<usize> = (0..self.candidates.len()).collect();
        order.sort_by(|a, b| scored[*b].partial_cmp(&scored[*a]).unwrap_or(std::cmp::Ordering::Equal));
        let mut taken: Vec<Option<Choice>> = self.candidates.drain(..).map(Some).collect();
        self.candidates = order
            .into_iter()
            .filter_map(|i| taken[i].take())
            .collect();
    }

    /// 送り仮名ごとの宛先で候補を並べ替える。
    ///
    /// **送りありの見出し語は、送り仮名が違っても同じ。** 「大きい」は `OoKii`、
    /// 「多く」は `OoKu` で、どちらも `おおk` を引く。見出し語だけで学習すると
    /// 片方がもう片方を引きずるので、確定のたびに送り仮名ごとの宛先も覚えてある。
    /// ここでそれを効かせる。
    ///
    /// **一致するものが無ければ何もしない。** 宛先が貯まっていない見出し語では
    /// 並びが変わらないので、この機能を入れて悪くなることがない。
    fn sort_by_okuri(&mut self) {
        if self.cfg.okuri_match == OkuriMatch::Off || self.okuri_kana.is_empty() {
            return;
        }
        let wanted = self.dict.okuri_candidates(&self.dict_key, &self.okuri_kana);
        if wanted.is_empty() {
            return;
        }
        match self.cfg.okuri_match {
            OkuriMatch::First => {
                // 安定な並べ替えなので、外れたものは元の順のまま後ろへ残る
                self.candidates.sort_by_key(|c| {
                    wanted
                        .iter()
                        .position(|t| *t == c.cand.text)
                        .unwrap_or(usize::MAX)
                });
            }
            OkuriMatch::Only => {
                // **空にはしない。** 宛先の候補が削除で消えているような場合に、
                // 全部落として辞書登録へ移ると、打った語を出せなくなる。
                if self.candidates.iter().any(|c| wanted.contains(&c.cand.text)) {
                    self.candidates.retain(|c| wanted.contains(&c.cand.text));
                }
            }
            OkuriMatch::Off => {}
        }
    }

    // ---- ▼ 候補選択 ----

    fn handle_selecting(&mut self, key: Key) -> Response {
        match key {
            k if self.cfg.cancel.contains(&k) => {
                self.phase = Phase::Composing;
                self.candidates.clear();
                self.cand_index = 0;
                Response::default()
            }
            // Enter を確定に固定する理由は handle_composing と同じ
            Key::Enter => {
                let text = self.commit_candidate();
                Response::text(&text)
            }
            k if self.cfg.confirm.contains(&k) => {
                let text = self.commit_candidate();
                Response::text(&text)
            }
            k if self.cfg.affix.contains(&k) => {
                // 接尾辞。いまの候補を確定し、`>` から始まる新しい見出し語を立てる。
                let text = self.commit_candidate();
                self.phase = Phase::Composing;
                self.set_reading(">".into());
                Response::text(&text)
            }
            k if self.cfg.purge.contains(&k) => {
                self.purge_candidate();
                Response::default()
            }
            k if self.cfg.convert.contains(&k) => {
                if self.next_candidate() {
                    // 候補を出し切ったので辞書登録へ
                    self.begin_registration();
                }
                Response::default()
            }
            k if self.cfg.previous.contains(&k) => {
                self.prev_candidate();
                Response::default()
            }
            Key::Char(c) if self.list_visible() && self.cfg.select.contains(&c) => {
                let (start, end) = self.page_range();
                let n = self.cfg.select.iter().position(|&k| k == c).unwrap();
                if start + n < end {
                    self.cand_index = start + n;
                    let text = self.commit_candidate();
                    return Response::text(&text);
                }
                Response::default()
            }
            k if self.cfg.backspace.contains(&k) => {
                self.prev_candidate();
                Response::default()
            }
            Key::Char(c) => {
                // 候補を確定してから、その文字を新しい入力として処理する
                let text = self.commit_candidate();
                let r = self.dispatch(Key::Char(c));
                Response {
                    commit: text + &r.commit,
                    passthrough: r.passthrough,
                    mode_changed: r.mode_changed,
                    cursor_back: r.cursor_back,
                    edit_snippet: r.edit_snippet,
                }
            }
            k => {
                let text = self.commit_candidate();
                Response {
                    commit: text,
                    passthrough: Some(k),
                    mode_changed: false,
                    cursor_back: 0,
                    edit_snippet: None,
                }
            }
        }
    }

    /// 次の候補へ。もう先が無ければ true (辞書登録へ移る合図)。
    fn next_candidate(&mut self) -> bool {
        if self.cand_index + 1 < self.cfg.inline_candidates {
            if self.cand_index + 1 >= self.candidates.len() {
                return true;
            }
            self.cand_index += 1;
            return false;
        }
        if !self.list_visible() {
            // 一覧の表示を始める
            if self.candidates.len() > self.cfg.inline_candidates {
                self.cand_index = self.cfg.inline_candidates;
                return false;
            }
            return true;
        }
        // 一覧が出ているときは頁単位で送る
        let (_, end) = self.page_range();
        if end < self.candidates.len() {
            self.cand_index = end;
            return false;
        }
        true
    }

    fn prev_candidate(&mut self) {
        if self.cand_index == 0 {
            self.phase = Phase::Composing;
            self.candidates.clear();
            return;
        }
        if !self.list_visible() {
            self.cand_index -= 1;
            return;
        }
        let (start, _) = self.page_range();
        self.cand_index = start
            .saturating_sub(self.cfg.page_size())
            .max(self.cfg.inline_candidates);
        if start == self.cfg.inline_candidates {
            self.cand_index = self.cfg.inline_candidates - 1;
        }
    }

    /// いま選んでいる候補を利用者辞書から取り除く。
    ///
    /// 候補が尽きたら ▽ に戻す。共有辞書には触れないので、そちら由来の候補は
    /// 次の変換でまた出る — 消えるのは学習による先頭への繰り上がりだけ。
    fn purge_candidate(&mut self) {
        let Some(c) = self.current_candidate() else {
            return;
        };
        let (key, text) = (c.key.clone(), c.cand.text.clone());
        self.dict.purge(&key, &text);
        self.candidates.retain(|c| c.cand.text != text);
        if self.candidates.is_empty() {
            self.phase = Phase::Composing;
            self.cand_index = 0;
        } else if self.cand_index >= self.candidates.len() {
            self.cand_index = self.candidates.len() - 1;
        }
    }

    fn commit_candidate(&mut self) -> String {
        let text = match self.current_candidate() {
            Some(c) => {
                let (key, cand) = (c.key.clone(), c.cand.clone());
                let shown = num::expand(&cand.text, &self.numbers);
                // 定型文の日付なども、出す側だけ開く。画面に見えている姿と
                // 子へ渡す姿を揃える。
                let shown = snippet::expand_variables(&shown, &self.now);
                // 辞書へ書き戻すのは `#` のままの形。数字を戻した形で覚えると
                // その数字専用の項目になってしまう。
                self.dict.learn(&key, &cand);
                let okuri = self.okuri_head.is_some();
                // 送り仮名ごとの宛先へも覚える。**見出し語だけでは足りない** —
                // 「おおきい」も「おおく」も おおk なので、片方の学習がもう片方を
                // 引きずる。
                let okuri_kana = self.okuri_kana.clone();
                if okuri {
                    self.dict.learn_okuri(&key, &okuri_kana, &cand.text);
                }

                // 埋める場所があるなら、確定せずに埋める段へ移る。**定型文の候補
                // だけを見る** — TextMate の決まりでは `$100` も埋め場所なので、
                // 共有辞書に `$` を含む候補があっても巻き込まない。
                if self.dict.is_snippet(&key, &cand.text)
                    && let Some(f) = Filling::new(&shown)
                {
                    let suffix = format!("{}{}", self.okuri_kana, self.auto_suffix);
                    self.note_commit(self.dict_key.clone(), shown, okuri);
                    self.reset();
                    self.filling = Some(f);
                    self.fill_suffix = suffix;
                    return String::new();
                }

                self.note_commit(self.dict_key.clone(), shown.clone(), okuri);
                format!("{}{}{}", shown, self.okuri_kana, self.auto_suffix)
            }
            None => String::new(),
        };
        self.reset();
        text
    }

    /// 埋めている最中のキー。
    ///
    /// 打った文字はいまの場所へ溜まる (変換も使えるので日本語をそのまま埋められる)。
    /// `TAB` で次へ、最後まで行くか `Enter` で組み上げて渡す。`C-g` は捨てる。
    fn handle_filling(&mut self, key: Key) -> Response {
        // 選ぶ場所では、変換キーで選択肢を回す
        let choices = self.filling.as_ref().expect("埋め中").choices();
        if !choices.is_empty()
            && (self.cfg.convert.contains(&key) || self.cfg.previous.contains(&key))
        {
            let f = self.filling.as_mut().expect("埋め中");
            let at = choices.iter().position(|c| c == f.value()).unwrap_or(0);
            let next = if self.cfg.previous.contains(&key) {
                (at + choices.len() - 1) % choices.len()
            } else {
                (at + 1) % choices.len()
            };
            *f.value_mut() = choices[next].clone();
            return Response::default();
        }

        if key == Key::Tab || key == Key::ShiftTab {
            self.settle_into_the_slot();
            let f = self.filling.as_mut().expect("埋め中");
            if key == Key::ShiftTab {
                f.at = f.at.saturating_sub(1);
                return Response::default();
            }
            if f.at + 1 < f.order.len() {
                f.at += 1;
                return Response::default();
            }
            return self.finish_filling();
        }
        if key == Key::Enter || self.cfg.confirm.contains(&key) {
            // 変換の途中なら、まずその変換を確定する。ここを分けないと**埋めながら
            // 日本語を変換できない** — 候補を決めるキーと埋め終わりのキーが同じ
            // なので、一つめを変換した時点で全体が出てしまう。
            if self.phase != Phase::Direct || !self.romaji.is_empty() {
                self.settle_into_the_slot();
                return Response::default();
            }
            return self.finish_filling();
        }
        if self.cfg.cancel.contains(&key) && self.romaji.is_empty() {
            self.filling = None;
            self.fill_suffix.clear();
            return Response::default();
        }
        if self.cfg.backspace.contains(&key) && self.romaji.is_empty() {
            let f = self.filling.as_mut().expect("埋め中");
            f.value_mut().pop();
            return Response::default();
        }

        // それ以外は普通に打つ。出るはずだった文字をいまの場所へ回す。
        let r = self.dispatch(key);
        if !r.commit.is_empty() {
            let commit = r.commit.clone();
            self.filling
                .as_mut()
                .expect("埋め中")
                .value_mut()
                .push_str(&commit);
        }
        // 埋めている最中に未知語へ当たっても、辞書登録までは連れて行かない。
        // 見出し語をそのまま値にして戻す — 定型文を埋めながら新しい語を覚える
        // 場面は考えにくいし、**登録の段を重ねると戻り道が分かりにくくなる**。
        if !self.regs.is_empty() {
            let reg = self.regs.pop().expect("登録中");
            let reading = reg.reading.clone();
            self.regs.clear();
            self.reset();
            self.filling
                .as_mut()
                .expect("埋め中")
                .value_mut()
                .push_str(&reading);
        }
        Response {
            commit: String::new(),
            passthrough: None,
            mode_changed: r.mode_changed,
            cursor_back: 0,
            edit_snippet: None,
        }
    }

    /// 打ちかけのものを、いま埋めている場所へ落とす。
    ///
    /// ▽ や ▼ のまま次へ移ろうとしたら、まず確定させる。打ちかけのローマ字も閉じる。
    /// これをしないと、変換中の候補が黙って消える。
    fn settle_into_the_slot(&mut self) {
        let mut settled = String::new();
        if self.phase != Phase::Direct {
            settled.push_str(&self.dispatch(Key::Enter).commit);
        }
        let flushed = self.romaji.flush();
        if !flushed.is_empty() {
            settled.push_str(&self.shape(&flushed));
        }
        if !settled.is_empty() {
            self.filling
                .as_mut()
                .expect("埋め中")
                .value_mut()
                .push_str(&settled);
        }
    }

    /// 埋め終わり。組み上げて子へ渡す。
    fn finish_filling(&mut self) -> Response {
        let f = self.filling.take().expect("埋め中");
        let (text, back) = f.build();
        let suffix = std::mem::take(&mut self.fill_suffix);
        self.reset();
        Response {
            commit: format!("{text}{suffix}"),
            passthrough: None,
            mode_changed: false,
            cursor_back: 0,
            edit_snippet: None,
        }
        .with_cursor_back(back)
    }
}

/// 解釈しないキーを元のバイト列に戻す。
fn raw_bytes(k: &Key) -> Vec<u8> {
    match k {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Ctrl(b) => vec![*b],
        Key::Enter => vec![0x0d],
        Key::Backspace => vec![0x7f],
        Key::Tab => vec![0x09],
        Key::ShiftTab => SHIFT_TAB.to_vec(),
        Key::Esc => vec![0x1b],
        Key::Raw(v) => v.clone(),
        // 括弧付き貼り付けは子アプリも括弧で受け取る前提なので、囲みごと組み直す
        Key::Paste(v) => {
            let mut out = Vec::with_capacity(v.len() + PASTE_START.len() + PASTE_END.len());
            out.extend_from_slice(PASTE_START);
            out.extend_from_slice(v);
            out.extend_from_slice(PASTE_END);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 試験は並列に走るので、辞書ファイルは呼び出しごとに別の場所に作る。
    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn skk_with(entries: &[(&str, &str)]) -> Skk {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ttyskk-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = dir.join("sys.dict");
        let mut body = String::from(";; okuri-nasi entries.\n");
        for (k, v) in entries {
            body.push_str(&format!("{} {}\n", k, v));
        }
        std::fs::write(&sys, body).unwrap();
        let dict = Dict::load(&[sys], dir.join("user.dict"), None).unwrap();
        Skk::new(dict, Config::default())
    }

    fn typed(skk: &mut Skk, s: &str) -> String {
        let mut out = Vec::new();
        for c in s.chars() {
            let key = match c {
                '\n' => Key::Ctrl(0x0a),
                '\r' => Key::Enter,
                '\x07' => Key::Ctrl(0x07),
                '\x7f' => Key::Backspace,
                c => Key::Char(c),
            };
            out.extend(skk.handle(key).to_child());
        }
        String::from_utf8(out).unwrap()
    }

    /// 前置キーを `;` に割り当てた Skk。
    ///
    /// **既定は割り当てなし**なので、前置キーを試すものはここから作る。
    fn skk_with_sticky(entries: &[(&str, &str)]) -> Skk {
        let mut skk = skk_with(entries);
        skk.set_config(Config {
            sticky: vec![Key::Char(';')],
            ..Config::default()
        });
        skk
    }

    /// 打ち込み中の内容だけ。モードの印は編集の対象ではないので外す。
    fn preedit_text(skk: &Skk) -> String {
        let p = skk.preedit();
        p.at_cursor
            .into_iter()
            .chain(p.floating)
            .filter(|s| {
                !matches!(
                    s.style,
                    Style::ModeHiragana
                        | Style::ModeKatakana
                        | Style::ModeHankaku
                        | Style::ModeZenkaku
                )
            })
            .map(|s| s.text)
            .collect()
    }

    /// ▽ の中でカーソルが乗っている一文字。末尾にいるときは空。
    fn cursor_char(skk: &Skk) -> String {
        skk.preedit()
            .at_cursor
            .into_iter()
            .filter(|s| s.style == Style::ReadingCursor)
            .map(|s| s.text)
            .collect()
    }

    /// モードの印だけ。
    fn mode_marker(skk: &Skk) -> String {
        skk.preedit()
            .at_cursor
            .into_iter()
            .filter(|s| {
                matches!(
                    s.style,
                    Style::ModeHiragana
                        | Style::ModeKatakana
                        | Style::ModeHankaku
                        | Style::ModeZenkaku
                )
            })
            .map(|s| s.text)
            .collect()
    }

    /// 前置キーを押した時点で、どこを切るのかが画面に出る。
    ///
    /// **押した手応えが無いと、効いたのかどうか分からない。** 印は ddskk が実際に
    /// 入れるものと同じ (`▽` = ここから読み、`*` = ここから送り仮名)。
    #[test]
    fn sticky_shows_where_it_will_cut() {
        let mut skk = skk_with_sticky(&[("かんがe", "/考/")]);
        skk.handle(Key::Ctrl(0x0a));

        typed(&mut skk, ";");
        assert_eq!(preedit_text(&skk), "▽");
        typed(&mut skk, "kanga");
        assert_eq!(preedit_text(&skk), "▽かんが");

        typed(&mut skk, ";");
        assert_eq!(preedit_text(&skk), "▽かんが*");
        typed(&mut skk, "e");
        assert_eq!(preedit_text(&skk), "▼考え");

        // ▼ で押したときは、確定して次の見出し語に入る予告
        typed(&mut skk, ";");
        assert_eq!(preedit_text(&skk), "▼考え▽");
        typed(&mut skk, "kanga");
        assert_eq!(preedit_text(&skk), "▽かんが");

        // 打ちかけのローマ字があっても、印は読みの頭に付く
        skk.handle(Key::Ctrl(0x07));
        typed(&mut skk, "k;");
        assert_eq!(preedit_text(&skk), "▽k");
    }

    /// 前置キーと Shift の結果は、**打ちかけのローマ字があっても**食い違わない。
    ///
    /// ddskk はここを取りこぼした前例がある (skk-dev/ddskk#197 — `;has;su` と打つと
    /// 打ちかけの `s` が消え、`HasSuru` と結果が変わる)。読み替えを打鍵の入口だけで
    /// 済ませ、下流を一切分岐させていないので、両者がずれる余地が無い。
    #[test]
    fn sticky_and_shift_agree_even_mid_romaji() {
        let dict = &[("はしr", "/走/"), ("かんじ", "/漢字/")];
        for (shift, sticky) in [
            ("HasSuru", ";has;suru"), // 送り仮名の頭が促音になる (#197 の打鍵列)
            ("HasiRu", ";hasi;ru"),
            ("Kanji ", ";kanji "),
        ] {
            let mut a = skk_with_sticky(dict);
            a.handle(Key::Ctrl(0x0a));
            let out_a = typed(&mut a, shift);
            let mut b = skk_with_sticky(dict);
            b.handle(Key::Ctrl(0x0a));
            let out_b = typed(&mut b, sticky);
            assert_eq!(
                (out_a, preedit_text(&a)),
                (out_b, preedit_text(&b)),
                "{shift} と {sticky} で結果が違う"
            );
        }
    }

    /// 前置キー (sticky) は Shift の同時押しと同じ結果になる。
    #[test]
    fn sticky_takes_the_place_of_shift() {
        let mut skk = skk_with_sticky(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, ";kanji");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼漢字");
    }

    /// 送り仮名の始まりも同じ手で示せる (`UgoKu` と打ったのと同じ)。
    #[test]
    fn sticky_starts_the_okuri() {
        let mut skk = skk_with_sticky(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, ";ugo;ku");
        assert_eq!(preedit_text(&skk), "[登録:うご*く]");
    }

    /// 二度押せばその文字が出る。記号は大文字にしても変わらないため。
    #[test]
    fn sticky_pressed_twice_types_the_character() {
        let mut skk = skk_with_sticky(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, ";;"), ";");
        assert!(preedit_text(&skk).is_empty());
    }

    /// かなモードの外では前置しない。あそこのキーは子アプリの持ち物。
    #[test]
    fn sticky_does_not_reach_the_ascii_modes() {
        let mut skk = skk_with_sticky(&[]);
        assert_eq!(typed(&mut skk, ";a"), ";a");
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "L");
        assert_eq!(typed(&mut skk, ";a"), "；ａ");
    }

    /// `/` の ASCII 見出し語でも前置しない。あそこは記号をそのまま打つ場所。
    #[test]
    fn sticky_does_not_reach_the_ascii_reading() {
        let mut skk = skk_with_sticky(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "/c;;");
        assert_eq!(preedit_text(&skk), "▽c;;");
    }

    /// 前置キーを持っている間は打ちかけと同じ。ここで ASCII へ降ろされると、
    /// 次の打鍵が大文字のまま子アプリへ抜けてしまう。
    #[test]
    fn sticky_is_not_idle() {
        let mut skk = skk_with_sticky(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, ";");
        assert!(!skk.leave_to_ascii());
        assert_eq!(typed(&mut skk, "ka"), "");
        assert_eq!(preedit_text(&skk), "▽か");
    }

    /// 既定では割り当てが無いので、`;` はただの記号のまま。
    #[test]
    fn sticky_is_off_until_it_is_asked_for() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, ";k"), ";");
        // `;` はその場で出て、`k` は打ちかけのローマ字として残る
        assert_eq!(preedit_text(&skk), "k");
    }

    #[test]
    fn unknown_word_starts_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 候補が無いので登録に入る。見出しが出て、入力は直接入力に戻る。
        assert_eq!(preedit_text(&skk), "[登録:かんじ]");

        // 登録内容を打つ。子へは何も出ない。
        assert_eq!(typed(&mut skk, "kanji"), "");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]かんじ");

        // Enter で確定。ここで初めて子へ出る。
        assert_eq!(typed(&mut skk, "\r"), "かんじ");
        assert!(preedit_text(&skk).is_empty());

        // 覚えたので次からは変換できる
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼かんじ");
    }

    #[test]
    fn registration_keeps_conversion_available_inside() {
        let mut skk = skk_with(&[("かん", "/漢/"), ("じ", "/字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 登録の中でも変換できる
        typed(&mut skk, "Kan \nJi \n");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]漢字");
        assert_eq!(typed(&mut skk, "\r"), "漢字");
    }

    #[test]
    fn registration_with_okuri_registers_only_the_stem() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "UgoKu");
        // 送りありは見出しに送り仮名が添う
        assert_eq!(preedit_text(&skk), "[登録:うご*く]");
        // l で ASCII にして漢字を直接打ち込む
        typed(&mut skk, "l動");
        assert_eq!(preedit_text(&skk), "[登録:うご*く]動");
        // 登録されるのは語幹だけ。子へ出るのは送り仮名の付いた形。
        assert_eq!(typed(&mut skk, "\r"), "動く");
        typed(&mut skk, "\nUgoKu");
        assert_eq!(preedit_text(&skk), "▼動く");
    }

    #[test]
    fn cancelling_registration_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "aiu");
        // C-g で登録を取り消すと ▽ に戻り、見出し語は残る
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn empty_registration_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(typed(&mut skk, "\r"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn backspace_walks_out_of_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あ");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]");
        // 空のところでもう一度押すと登録から抜ける
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn exhausting_the_candidates_starts_registration() {
        let mut skk = skk_with(&[("あい", "/愛/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai ");
        assert_eq!(preedit_text(&skk), "▼愛");
        // 候補を出し切ったところで space を押すと登録へ
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "[登録:あい]");
    }

    #[test]
    fn registration_nests() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 登録の中でさらに未知語を変換すると、もう一段積まれる
        typed(&mut skk, "Ai ");
        assert_eq!(preedit_text(&skk), "[[登録:あい]]");
        typed(&mut skk, "l愛");
        assert_eq!(typed(&mut skk, "\r"), "");
        // 内側が確定すると外側の登録内容になる
        assert_eq!(preedit_text(&skk), "[登録:かんじ]愛");
        assert_eq!(typed(&mut skk, "\r"), "愛");
    }

    #[test]
    fn control_keys_do_not_leak_into_the_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        // 割り当ての無い C-z や矢印は登録内容に混ざらない
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child(), Vec::<u8>::new());
        assert_eq!(
            skk.handle(Key::Raw(b"\x1b[A".to_vec())).to_child(),
            Vec::<u8>::new()
        );
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
    }

    /// AZIK の `c` (チャ行の子音) と、丸数字の読み `c1` は食い違わない。
    ///
    /// `c` の後ろに数字が来る綴りは AZIK にも標準にも無いので、`c` はそのまま
    /// 見出し語に落ちる。かなにならない文字を素通しする既存の道筋がそのまま働く。
    #[test]
    fn azik_does_not_eat_the_circled_number_reading() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config {
            azik: true,
            ..Default::default()
        });
        skk.handle(Key::Ctrl(0x0a));

        typed(&mut skk, "C1");
        assert_eq!(preedit_text(&skk), "▽c1");
        assert_eq!(typed(&mut skk, " \n"), "①");

        // AZIK の綴りとしての c は従来どおり (直接入力なのでその場で出る)
        assert_eq!(typed(&mut skk, "ca"), "ちゃ");
        // まる1 の側も打てる
        typed(&mut skk, "Maru1");
        assert_eq!(preedit_text(&skk), "▽まる1");
        assert_eq!(typed(&mut skk, " \n"), "①");
    }

    /// 区切りの文字が来たら、その手前までで自動的に変換を始める。
    ///
    /// 「ほんやくを」と打つと `を` の直前で変換に入り、`を` は候補の後ろに置かれる。
    /// **引き金の文字は見出し語に含めない** — 含めると辞書を引けない。
    #[test]
    fn auto_start_henkan_converts_before_the_keyword() {
        let mut skk = skk_with(&[("ほんやく", "/翻訳/飜訳/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Honnyaku");
        assert_eq!(preedit_text(&skk), "▽ほんやく");

        // 「を」で変換が始まり、「を」は候補の後ろに付く
        typed(&mut skk, "wo");
        assert_eq!(preedit_text(&skk), "▼翻訳を");
        // 候補を送っても「を」はそのまま残る
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼飜訳を");
        assert_eq!(typed(&mut skk, "\n"), "飜訳を");
    }

    /// 句点でも始まる。撥音と一度に出るときは「ん」までが見出し語。
    #[test]
    fn auto_start_henkan_keeps_the_kana_before_the_keyword() {
        let mut skk = skk_with(&[("にほん", "/日本/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihon");
        // 「n」はまだローマ字のまま。「.」で「ん」と「。」が一度に出る
        assert_eq!(preedit_text(&skk), "▽にほn");
        typed(&mut skk, ".");
        assert_eq!(preedit_text(&skk), "▼日本。");
        assert_eq!(typed(&mut skk, "\n"), "日本。");
    }

    /// 候補が無ければ登録へ移り、登録を終えると引き金の文字が後ろに付く。
    #[test]
    fn auto_start_henkan_survives_the_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Honnyaku");
        typed(&mut skk, "wo");
        assert_eq!(preedit_text(&skk), "[登録:ほんやく]");
        typed(&mut skk, "honnyaku");
        assert_eq!(preedit_text(&skk), "[登録:ほんやく]ほんやく");
        assert_eq!(typed(&mut skk, "\r"), "ほんやくを");
    }

    /// 見出し語が空のときは、ただの文字として入る。
    #[test]
    fn auto_start_henkan_needs_a_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "wo"), "を");
        // ▽ を始めた直後も同じ
        typed(&mut skk, "Q");
        assert_eq!(preedit_text(&skk), "▽");
        typed(&mut skk, "wo");
        assert_eq!(preedit_text(&skk), "▽を");
    }

    /// 空の並びを設定すると自動変換をしない。
    #[test]
    fn auto_start_henkan_can_be_turned_off() {
        let mut skk = skk_with(&[("ほんやく", "/翻訳/")]);
        let mut cfg = Config::default();
        cfg.auto_start_henkan.clear();
        skk.set_config(cfg);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Honnyakuwo");
        assert_eq!(preedit_text(&skk), "▽ほんやくを");
    }

    /// abbrev (ASCII 見出し語) では働かない。記号がそのまま見出し語になる。
    #[test]
    fn auto_start_henkan_stays_out_of_abbrev() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "/ab.");
        assert_eq!(preedit_text(&skk), "▽ab.");
    }

    /// 確定した文字列と、解釈しなかったキーは別々に取れる。
    ///
    /// 端末では一本のバイト列に組んで子へ流すが、GUI の入力メソッドでは前者が
    /// 「文字列の確定」、後者が「このキーは使わなかった」というまったく別の知らせ
    /// になる。混ぜてしまうと受け取った側で分けられない。
    #[test]
    fn commit_and_passthrough_come_apart() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");

        // ▽ の途中で矢印。見出し語を確定したうえで矢印を渡す
        let r = skk.handle(Key::Raw(b"\x1b[A".to_vec()));
        assert_eq!(r.commit, "かんじ");
        assert_eq!(r.passthrough, Some(Key::Raw(b"\x1b[A".to_vec())));
        // 端末向けには一本に組める
        assert_eq!(r.to_child(), "かんじ\x1b[A".as_bytes());

        // 確定だけの場合は素通しが無い
        let r = skk.handle(Key::Char('a'));
        assert_eq!((r.commit.as_str(), r.passthrough), ("あ", None));
    }

    /// 登録の途中で ASCII モードにして打った英字も、登録内容に入る。
    ///
    /// ASCII モードの文字は「解釈しなかったキー」として返るので、素通しの側も
    /// 見ないと取りこぼす。
    #[test]
    fn ascii_typing_reaches_the_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]");
        typed(&mut skk, "l");
        typed(&mut skk, "abc");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]abc");
        assert_eq!(typed(&mut skk, "\r"), "abc");
    }

    /// 候補は端末向けの組み上げを通さず、そのまま取り出せる。
    #[test]
    fn candidates_come_out_as_data() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事;宴会の/")]);
        skk.handle(Key::Ctrl(0x0a));
        // 変換していない間は無い
        assert!(skk.candidates().is_none());

        typed(&mut skk, "Kanji ");
        let v = skk.candidates().expect("▼ の最中");
        assert_eq!(v.items.len(), 2);
        assert_eq!(v.items[0].text, "漢字");
        assert_eq!(v.items[0].annotation, None);
        assert_eq!(v.items[1].text, "幹事");
        assert_eq!(v.items[1].annotation.as_deref(), Some("宴会の"));
        assert_eq!(v.selected, 0);
        assert_eq!(v.select_keys, Config::default().select);

        // 次の候補へ送ると選択位置が動く
        typed(&mut skk, " ");
        assert_eq!(skk.candidates().unwrap().selected, 1);

        // 確定したら消える
        typed(&mut skk, "\n");
        assert!(skk.candidates().is_none());
    }

    /// 数値変換の展開も済んだ形で出る。
    #[test]
    fn candidates_show_the_expanded_numbers() {
        let mut skk = skk_with(&[("だい#かい", "/第#1回/第#3回/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Dai5kai ");
        let v = skk.candidates().unwrap();
        // #1 は全角、#3 は位取りの漢数字
        assert_eq!(v.items[0].text, "第５回");
        assert_eq!(v.items[1].text, "第五回");
    }

    /// 貼り付けを一つ与えて、子へ出るものを見る。
    ///
    /// 端末のバイト列から `Key::Paste` を切り出すところは実行ファイル側の仕事なので、
    /// ここではキーを直に渡す (切り出しは `input` のテストが見ている)。
    fn pasted(skk: &mut Skk, text: &str) -> String {
        let r = skk.handle(Key::Paste(text.as_bytes().to_vec()));
        String::from_utf8(r.to_child()).unwrap()
    }

    /// 貼り付けた中身はローマ字変換にもモード切り替えにも回さない。
    ///
    /// 直さないと `hello` が「へ」+ ASCII モードへの `l` + `lo` になる。
    #[test]
    fn pasted_text_is_not_converted() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(
            pasted(&mut skk, "hello, world"),
            "\x1b[200~hello, world\x1b[201~"
        );
        // モードも動いていない
        assert_eq!(skk.mode, Mode::Hiragana);
        // 続きの打鍵はこれまでどおり変換される
        assert_eq!(typed(&mut skk, "ai"), "あい");
    }

    /// 変換の途中で貼ったら、先に見出し語・候補を確定してから貼り付けを流す。
    #[test]
    fn pasting_confirms_what_is_being_converted() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        assert_eq!(pasted(&mut skk, "x"), "かんじ\x1b[200~x\x1b[201~");
        assert_eq!(preedit_text(&skk), "");

        typed(&mut skk, "Kanji ");
        assert_eq!(pasted(&mut skk, "x"), "漢字\x1b[200~x\x1b[201~");
        assert_eq!(preedit_text(&skk), "");
    }

    /// 候補が無くて登録に入ったところで、定型文にする道を出す。
    ///
    /// 覚えるキーを増やさないために、**何も打っていないところでの変換キー**に載せて
    /// いる。そこはもともと半角空白を溜めるだけの位置。
    #[test]
    fn offers_to_write_a_snippet_from_the_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kaisyadenwa ");
        assert_eq!(preedit_text(&skk), "[登録:かいしゃでんわ]");

        // 何も打っていないところで変換キー
        skk.handle(Key::Char(' '));
        assert_eq!(
            preedit_text(&skk),
            "[登録:かいしゃでんわ]▼[スニペットを登録]"
        );

        // 決めると、見出し語を添えて呼ぶ側へ頼む
        let r = skk.handle(Key::Enter);
        assert_eq!(r.edit_snippet.as_deref(), Some("かいしゃでんわ"));
        assert!(r.commit.is_empty(), "子へは何も出さない");
        // 登録は畳んで ▽ に戻る (定型文として書くので辞書には入れない)
        assert_eq!(preedit_text(&skk), "▽かいしゃでんわ");
    }

    /// 出したあとに打ち始めたら引っ込めて、そのキーを普通に処理する。
    #[test]
    fn typing_dismisses_the_snippet_offer() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        skk.handle(Key::Char(' '));
        assert!(preedit_text(&skk).contains("スニペット"));

        // 打ち始めれば消えて、打った文字は登録内容に入る
        typed(&mut skk, "ai");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
    }

    /// 定型文に埋める場所があると、確定せずに順に埋める段へ移る。
    #[test]
    fn fills_the_placeholders_in_order() {
        let dir = std::env::temp_dir().join(format!("ttyskk-fill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(
            &snip,
            r#"{"挨拶": {"prefix": "あいさつ", "body": "${1:宛先} 様、$2 です。"}}"#,
        )
        .unwrap();
        let mut skk = skk_with(&[("たなか", "/田中/"), ("たけうち", "/竹内/")]);
        skk.dict_mut().load_snippets(std::slice::from_ref(&snip));

        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Aisatu ");
        // 埋める場所は既定値を入れた姿で見せる (注釈は項目の名前)
        assert_eq!(preedit_text(&skk), "▼宛先 様、 です。 ; 挨拶");

        // 確定すると、子へは出さずに埋める段へ移る
        assert_eq!(typed(&mut skk, "\r"), "");
        assert_eq!(preedit_text(&skk), "[埋め 1/2]宛先 様、 です。");

        // 打った文字はいまの場所へ。既定値は打ち直しで消える
        skk.handle(Key::Backspace);
        skk.handle(Key::Backspace);
        typed(&mut skk, "Tanaka ");
        // 打ちかけのものは、埋める場所に差し込んで見せる
        assert_eq!(preedit_text(&skk), "[埋め 1/2]▼田中 様、 です。");

        // TAB で確定して次へ
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "[埋め 2/2]田中 様、 です。");
        typed(&mut skk, "Takeuti ");
        // 最後の TAB で組み上げて渡す
        let r = skk.handle(Key::Tab);
        assert_eq!(r.commit, "田中 様、竹内 です。");
        assert_eq!(preedit_text(&skk), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同じ番号は同じ値になり、`$0` はカーソルを戻す幅になる。
    #[test]
    fn mirrors_the_same_number_and_reports_the_final_position() {
        let dir = std::env::temp_dir().join(format!("ttyskk-fill2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(
            &snip,
            r#"{"括弧": {"prefix": "かっこ", "body": "「$1」$0 と$1"}}"#,
        )
        .unwrap();
        let mut skk = skk_with(&[("あい", "/愛/")]);
        skk.dict_mut().load_snippets(std::slice::from_ref(&snip));

        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kakko ");
        typed(&mut skk, "\r");
        typed(&mut skk, "Ai ");
        // 変換の途中の Enter は、その変換を決めるだけ (埋め終わりにはしない)
        assert_eq!(skk.handle(Key::Enter).commit, "");
        let r = skk.handle(Key::Enter);
        assert_eq!(r.commit, "「愛」 と愛", "同じ番号は同じ値");
        assert_eq!(r.cursor_back, 3, "$0 から末尾までの 3 文字ぶん戻す");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 選ぶ場所は変換キーで回す。
    #[test]
    fn cycles_through_the_choices() {
        let dir = std::env::temp_dir().join(format!("ttyskk-fill3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(
            &snip,
            r#"{"返事": {"prefix": "へんじ", "body": "${1|承知しました,検討します|}。"}}"#,
        )
        .unwrap();
        let mut skk = skk_with(&[]);
        skk.dict_mut().load_snippets(std::slice::from_ref(&snip));

        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Henzi ");
        typed(&mut skk, "\r");
        assert_eq!(preedit_text(&skk), "[埋め 1/1]承知しました。 ; 2 択");
        skk.handle(Key::Char(' '));
        assert_eq!(preedit_text(&skk), "[埋め 1/1]検討します。 ; 2 択");
        // 一周する
        skk.handle(Key::Char(' '));
        assert_eq!(preedit_text(&skk), "[埋め 1/1]承知しました。 ; 2 択");
        assert_eq!(skk.handle(Key::Enter).commit, "承知しました。");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 共有辞書の候補は、`$` を含んでいても埋める段へ入らない。
    ///
    /// TextMate の決まりでは `$100` も埋め場所なので、限らないと巻き込む。
    #[test]
    fn only_snippets_enter_the_filling_stage() {
        let mut skk = skk_with(&[("ねだん", "/$100/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nedan ");
        assert_eq!(typed(&mut skk, "\r"), "$100", "そのまま確定する");
        assert_eq!(preedit_text(&skk), "");
    }

    /// 埋めている最中に未知語へ当たっても、辞書登録までは連れて行かない。
    ///
    /// 登録の段を重ねると戻り道が分かりにくくなる。読みをそのまま値にして続ける。
    #[test]
    fn an_unknown_word_while_filling_falls_back_to_the_reading() {
        let dir = std::env::temp_dir().join(format!("ttyskk-fill5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(&snip, r#"{"礼": {"prefix": "れい", "body": "$1 さんへ"}}"#).unwrap();
        let mut skk = skk_with(&[]);
        skk.dict_mut().load_snippets(std::slice::from_ref(&snip));

        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Rei ");
        typed(&mut skk, "\r");
        // 辞書に無い語を変換しようとする
        typed(&mut skk, "Tanaka ");
        assert_eq!(preedit_text(&skk), "[埋め 1/1]たなか さんへ", "読みが入る");
        assert_eq!(skk.handle(Key::Enter).commit, "たなか さんへ");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 埋めるのをやめたら、何も出さずに消える。
    #[test]
    fn cancelling_the_filling_emits_nothing() {
        let dir = std::env::temp_dir().join(format!("ttyskk-fill4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(&snip, r#"{"礼": {"prefix": "れい", "body": "$1 さんへ"}}"#).unwrap();
        let mut skk = skk_with(&[]);
        skk.dict_mut().load_snippets(std::slice::from_ref(&snip));

        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Rei ");
        typed(&mut skk, "\r");
        assert!(preedit_text(&skk).starts_with("[埋め"));
        let r = skk.handle(Key::Ctrl(0x07));
        assert_eq!(r.commit, "");
        assert_eq!(preedit_text(&skk), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 定型文に書いた日付は、候補に出る時点でもう開いている。
    ///
    /// 確定してから気付くのではなく、**選んでいる最中に確かめられる**。
    #[test]
    fn the_date_is_already_expanded_in_the_candidate() {
        let mut skk = skk_with(&[("きょう", "/$CURRENT_YEAR-$CURRENT_MONTH-$CURRENT_DATE/")]);
        skk.set_now(crate::snippet::Now {
            year: 2026,
            month: 7,
            day: 28,
            weekday: 2,
            ..Default::default()
        });
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kyou ");
        assert_eq!(preedit_text(&skk), "▼2026-07-28");
        assert_eq!(typed(&mut skk, "\r"), "2026-07-28");
    }

    /// 時計を教えられていなければ、書いたままの姿で出る (壊れはしない)。
    #[test]
    fn candidates_survive_without_a_clock() {
        let mut skk = skk_with(&[("ねだん", "/$100/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nedan ");
        assert_eq!(preedit_text(&skk), "▼$100", "知らない名前は残す");
    }

    /// 割り当てれば、変換に入る前からでも定型文を書きに行ける。
    ///
    /// 既定は空なので、何も設定しなければ普通の文字として扱われる。
    #[test]
    fn the_snippet_key_works_from_anywhere_when_bound() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[keys]\nsnippet_edit = \"C-t\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));

        // 何も打っていないところから。見出し語は決まっていない
        let r = skk.handle(Key::Ctrl(0x14));
        assert_eq!(r.edit_snippet.as_deref(), Some(""));

        // ▽ の途中なら、打っている見出し語を持って行く
        typed(&mut skk, "Kaisya");
        let r = skk.handle(Key::Ctrl(0x14));
        assert_eq!(r.edit_snippet.as_deref(), Some("かいしゃ"));

        // ASCII モードでは効かない (子アプリの持ち物なので奪わない)
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[keys]\nsnippet_edit = \"C-t\"\n").unwrap());
        let r = skk.handle(Key::Ctrl(0x14));
        assert_eq!(r.edit_snippet, None);
        assert_eq!(r.passthrough, Some(Key::Ctrl(0x14)), "子へ渡す");
    }

    /// 割り当てが無ければ、そのキーはこれまでどおり素通しする。
    #[test]
    fn the_snippet_key_is_unbound_by_default() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x14));
        assert_eq!(r.edit_snippet, None);
        assert_eq!(r.passthrough, Some(Key::Ctrl(0x14)));
    }

    /// 取り消しは誘いを引っ込めるだけで、登録そのものは畳まない。
    #[test]
    fn cancel_dismisses_only_the_offer() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        skk.handle(Key::Char(' '));
        assert!(preedit_text(&skk).contains("スニペット"));

        skk.handle(Key::Ctrl(0x07));
        assert_eq!(preedit_text(&skk), "[登録:かんじ]", "登録は続いている");
        // もう一度出せる
        skk.handle(Key::Char(' '));
        assert!(preedit_text(&skk).contains("スニペット"));
    }

    /// 何か打ったあとの変換キーは、これまでどおり登録内容の変換に使う。
    #[test]
    fn the_offer_only_appears_on_an_empty_registration() {
        let mut skk = skk_with(&[("あい", "/愛/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "Ai ");
        // ▼愛 が出る (スニペットの誘いではない)
        assert_eq!(preedit_text(&skk), "[登録:かんじ]▼愛");
    }

    /// 登録の途中では、貼り付けた中身がそのまま登録内容になる。
    #[test]
    fn pasting_into_the_registration_keeps_the_text() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(pasted(&mut skk, "漢字"), "");
        assert_eq!(preedit_text(&skk), "[登録:かんじ]漢字");
        assert_eq!(typed(&mut skk, "\r"), "漢字");
    }

    #[test]
    fn escape_returns_to_ascii() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "ai");
        // Esc は子へ渡りつつ、モードは ASCII に戻る (vim の挿入モードを抜ける動作)
        let r = skk.handle(Key::Esc);
        assert_eq!(r.to_child(), vec![0x1b]);
        assert!(r.mode_changed);
        assert_eq!(skk.mode, Mode::Ascii);
        // 以降はそのまま英字が通る
        assert_eq!(typed(&mut skk, "dd"), "dd");
    }

    #[test]
    fn escape_confirms_the_reading_first() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 変換中に Esc を押すと、候補を確定してから抜ける
        let r = skk.handle(Key::Esc);
        assert_eq!(String::from_utf8(r.to_child()).unwrap(), "漢字\u{1b}");
        assert_eq!(skk.mode, Mode::Ascii);
    }

    #[test]
    fn escape_does_not_disturb_registration() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        typed(&mut skk, "ai");
        skk.handle(Key::Esc);
        // 登録中は打ち込みが消えないよう、モードも内容もそのまま
        assert_eq!(skk.mode, Mode::Hiragana);
        assert_eq!(preedit_text(&skk), "[登録:かんじ]あい");
    }

    #[test]
    fn ctrl_c_also_returns_to_ascii() {
        // nvim では C-c も挿入モードを抜ける (C-d は抜けないので既定に入れない)
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x03));
        assert_eq!(r.to_child(), vec![0x03]);
        assert_eq!(skk.mode, Mode::Ascii);

        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x04));
        assert_eq!(r.to_child(), vec![0x04]);
        assert_eq!(skk.mode, Mode::Hiragana, "C-d では抜けない");
    }

    #[test]
    fn ascii_keys_can_be_turned_off() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nascii_keys = []\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Esc);
        assert_eq!(r.to_child(), vec![0x1b]);
        assert_eq!(skk.mode, Mode::Hiragana);
    }

    #[test]
    fn tab_completes_the_reading() {
        let mut skk = skk_with(&[
            ("かんじ", "/漢字/"),
            ("かんじゃ", "/患者/"),
            ("かんきょう", "/環境/"),
            ("かい", "/回/"),
        ]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        // 「n」はまだローマ字のまま。補完はこれを「ん」に確定してから探す。
        assert_eq!(preedit_text(&skk), "▽かn");

        // 短い順、同じ長さなら辞書順。**何になるか**と何番目かが添う。
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ 漢字 [1/3]");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじゃ 患者 [2/3]");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんきょう 環境 [3/3]");
        // 一周する
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ 漢字 [1/3]");
        // Shift+Tab で戻る
        skk.handle(Key::ShiftTab);
        assert_eq!(preedit_text(&skk), "▽かんきょう 環境 [3/3]");

        // 補完したものはそのまま変換できる
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼環境");
    }

    /// 補完に添えるものは、見出し語だけでは選べないから要る。
    #[test]
    fn completion_shows_what_it_becomes() {
        let mut skk = skk_with(&[
            ("かんじゃ", "/患者;病人/"),
            ("かんじゃく", "/閑寂/"),
            ("かんじ", "/漢字/"),
        ]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");

        // 注釈があれば ▼ のときと同じ形で続ける
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじゃ 患者 ; 病人 [1/2]");
        // 無ければ候補だけ
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじゃく 閑寂 [2/2]");

        // 補完を抜けたら添えない (完全一致の「かんじ」は補完の対象外)
        typed(&mut skk, "\x07");
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn cancel_undoes_the_completion() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ 漢字 [1/1]");
        // 一度目の C-g は補完を取り消すだけ。▽ は残る。
        typed(&mut skk, "\x07");
        assert_eq!(preedit_text(&skk), "▽かん");
        // 二度目で ▽ ごと取り消す
        typed(&mut skk, "\x07");
        assert!(preedit_text(&skk).is_empty());
    }

    #[test]
    fn typing_after_a_completion_starts_over() {
        let mut skk = skk_with(&[("かんじ", "/漢字/"), ("かんじゃ", "/患者/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kan");
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽かんじ 漢字 [1/2]");
        // 打ち足すと並びは作り直される
        typed(&mut skk, "ya");
        assert_eq!(preedit_text(&skk), "▽かんじや");
        skk.handle(Key::Tab);
        assert_eq!(
            preedit_text(&skk),
            "▽かんじや",
            "前方一致しないので変わらない"
        );
    }

    /// 動的補完。既定では出ないので、設定を入れた側だけで見える。
    #[test]
    fn dynamic_completion_shows_the_rest_while_typing() {
        let entries = &[
            ("にほんご", "/日本語/"),
            ("にほんじん", "/日本人/"),
            ("にほん", "/日本/"),
        ];
        let mut skk = skk_with(entries);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihonn");
        assert_eq!(preedit_text(&skk), "▽にほん", "既定では出さない");

        let mut skk = skk_with(entries);
        skk.set_config(Config::parse("[behavior]\ndynamic_completion = \"single\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihonn");
        // 打っていない「ご」が続く。短い順なので「にほんご」が先。
        assert_eq!(preedit_text(&skk), "▽にほんご");
        // 薄字なのは補った分だけ。打った分は見出し語のまま。
        let p = skk.preedit();
        assert_eq!(p.at_cursor[0].style, Style::Reading);
        assert_eq!(p.at_cursor[0].text, "▽にほん");
        assert_eq!(p.at_cursor[1].style, Style::Completion);
        assert_eq!(p.at_cursor[1].text, "ご");

        // 見せているだけなので、そのまま変換すれば打った分だけが対象になる
        let mut probe = skk_with(entries);
        probe.set_config(Config::parse("[behavior]\ndynamic_completion = \"single\"\n").unwrap());
        probe.handle(Key::Ctrl(0x0a));
        typed(&mut probe, "Nihonn ");
        assert_eq!(preedit_text(&probe), "▼日本");

        // TAB を押すと、見せていたものがそのまま見出し語に入る
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽にほんご 日本語 [1/2]");
    }

    #[test]
    fn dynamic_completion_can_list_several() {
        let mut skk = skk_with(&[
            ("にほんご", "/日本語/"),
            ("にほんじん", "/日本人/"),
            ("にほんかい", "/日本海/"),
        ]);
        skk.set_config(Config::parse("[behavior]\ndynamic_completion = \"multiple\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihonn");
        assert_eq!(preedit_text(&skk), "▽にほん にほんご にほんかい にほんじん");
        // 先頭が TAB で入るもの
        skk.handle(Key::Tab);
        assert_eq!(preedit_text(&skk), "▽にほんご 日本語 [1/3]");

        // 一覧の出し方は候補一覧に合わせる。float なら浮かせる行へ回る。
        let mut skk = skk_with(&[("にほんご", "/日本語/")]);
        skk.set_config(
            Config::parse(
                "[behavior]\ndynamic_completion = \"multiple\"\n[candidates]\nlayout = \"float\"\n",
            )
            .unwrap(),
        );
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihonn");
        let p = skk.preedit();
        assert_eq!(
            p.at_cursor
                .iter()
                .map(|s| s.text.clone())
                .collect::<String>(),
            "▽にほん"
        );
        assert_eq!(
            p.floating
                .iter()
                .map(|s| s.text.clone())
                .collect::<String>(),
            "にほんご"
        );
    }

    /// 伸ばす先が末尾でない間は出さない。出しても打ち込みと辻褄が合わない。
    #[test]
    fn dynamic_completion_keeps_quiet_where_it_cannot_extend() {
        let mut skk = skk_with(&[("にほんご", "/日本語/"), ("にほんかい", "/日本海/")]);
        skk.set_config(Config::parse("[behavior]\ndynamic_completion = \"single\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihonn");
        assert_eq!(preedit_text(&skk), "▽にほんご");

        // ローマ字が打ちかけの間は、続きが決まらない
        typed(&mut skk, "k");
        assert_eq!(preedit_text(&skk), "▽にほんk");
        typed(&mut skk, "a");
        assert_eq!(preedit_text(&skk), "▽にほんかい", "かなが揃えば出る");

        // 見出し語の途中へ戻ったら、伸ばす場所は末尾ではない
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "か");
        assert_eq!(preedit_text(&skk), "▽にほんか");

        // 送り仮名に入ったら見出し語は伸びない
        let mut skk = skk_with(&[("にほんご", "/日本語/")]);
        skk.set_config(Config::parse("[behavior]\ndynamic_completion = \"single\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "NihonnN");
        assert_eq!(preedit_text(&skk), "▽にほん*n");
    }

    #[test]
    fn tab_outside_conversion_reaches_the_child() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        // ASCII でもかなでも、直接入力中の TAB は子へ渡す (シェルの補完を殺さない)
        assert_eq!(skk.handle(Key::Tab).to_child(), vec![0x09]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(skk.handle(Key::Tab).to_child(), vec![0x09]);
    }

    #[test]
    fn purge_removes_the_learned_candidate() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        // 一度「幹事」を確定して学習させる
        typed(&mut skk, "Kanji  \n");
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼幹事", "学習で先頭に来ている");

        // X で取り除くと次の候補に移る
        typed(&mut skk, "X");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 学習による繰り上がりが消え、共有辞書の並びに戻る
        typed(&mut skk, "\n");
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
    }

    #[test]
    fn purging_the_last_candidate_returns_to_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // 登録した語をひとつだけ持つ状態にする
        typed(&mut skk, "Tegaki ");
        typed(&mut skk, "lXY\r");
        typed(&mut skk, "\nTegaki ");
        assert_eq!(preedit_text(&skk), "▼XY");
        // 唯一の候補を消すと ▽ に戻る
        typed(&mut skk, "X");
        assert_eq!(preedit_text(&skk), "▽てがき");
    }

    #[test]
    fn numeric_conversion() {
        let mut skk = skk_with(&[("だい#かい", "/第#1回/第#0回/第#3回/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Dai5kai ");
        assert_eq!(preedit_text(&skk), "▼第５回");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第5回");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第五回");
        assert_eq!(typed(&mut skk, "\n"), "第五回");

        // 別の数字でも同じ項目が効く
        typed(&mut skk, "Dai12kai ");
        assert_eq!(preedit_text(&skk), "▼第十二回", "学習した #3 が先頭に来る");
        assert_eq!(typed(&mut skk, "\n"), "第十二回");
    }

    #[test]
    fn numeric_learning_keeps_the_hash_form() {
        let mut skk = skk_with(&[("だい#かい", "/第#1回/第#3回/")]);
        skk.handle(Key::Ctrl(0x0a));
        // 二番目 (#3) を選んで確定する
        typed(&mut skk, "Dai5kai  \n");
        // 辞書に書き戻されたのは `#` のままの形。数字専用の項目にはならない。
        let cands = skk.dict_mut().lookup("だい#かい");
        assert_eq!(cands[0].text, "第#3回");
        assert!(skk.dict_mut().lookup("だい5かい").is_empty());
    }

    #[test]
    fn literal_entry_wins_over_the_hash_form() {
        let mut skk = skk_with(&[
            ("だい#かい", "/第#1回/"),
            ("だい5かい", "/第五回だけの項目/"),
        ]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Dai5kai ");
        // 打った通りの見出し語が先
        assert_eq!(preedit_text(&skk), "▼第五回だけの項目");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼第５回");
    }

    #[test]
    fn a_reading_without_digits_is_untouched() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        assert_eq!(typed(&mut skk, "\n"), "漢字");
    }

    #[test]
    fn prefix_conversion() {
        let mut skk = skk_with(&[("あか>", "/赤/"), ("ぺん", "/ペン/")]);
        skk.handle(Key::Ctrl(0x0a));
        // ▽ の途中で > を押すと末尾に付いてすぐ変換が始まる
        typed(&mut skk, "Aka>");
        assert_eq!(preedit_text(&skk), "▼赤");
        assert_eq!(typed(&mut skk, "\n"), "赤");
        assert_eq!(typed(&mut skk, "Pen \n"), "ペン");
    }

    #[test]
    fn suffix_conversion() {
        let mut skk = skk_with(&[("かんどう", "/感動/"), (">てき", "/的/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kandou ");
        assert_eq!(preedit_text(&skk), "▼感動");
        // ▼ で > を押すと候補を確定し、> から始まる新しい見出し語を立てる
        assert_eq!(typed(&mut skk, ">"), "感動");
        assert_eq!(preedit_text(&skk), "▽>");
        typed(&mut skk, "teki");
        assert_eq!(preedit_text(&skk), "▽>てき");
        assert_eq!(typed(&mut skk, " \n"), "的");
    }

    #[test]
    fn learns_the_combined_word() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\n");
        typed(&mut skk, "Riyou \n");
        // 接頭辞に続いた語を繋げて覚える (ddskk の skk-learn-combined-word)
        let c = skk.dict_mut().lookup("さいりよう");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "再利用");
    }

    #[test]
    fn learns_the_combined_word_with_a_suffix() {
        let mut skk = skk_with(&[("かんどう", "/感動/"), (">てき", "/的/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kandou >teki \n");
        let c = skk.dict_mut().lookup("かんどうてき");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "感動的");
    }

    #[test]
    fn does_not_combine_across_other_input() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\n");
        // 間にかなを打ったら隣り合っていない
        typed(&mut skk, "no");
        typed(&mut skk, "Riyou \n");
        assert!(skk.dict_mut().lookup("さいりよう").is_empty());
    }

    #[test]
    fn combining_can_be_turned_off() {
        let mut skk = skk_with(&[("さい>", "/再/"), ("りよう", "/利用/")]);
        skk.set_config(Config::parse("[behavior]\nlearn_combined = false\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>\nRiyou \n");
        assert!(skk.dict_mut().lookup("さいりよう").is_empty());
    }

    #[test]
    fn affix_marker_is_stripped_from_the_candidate() {
        // SKK-JISYO.L には候補側にも > が付く項目が 1 件ある
        let mut skk = skk_with(&[("さい>", "/再>/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Sai>");
        assert_eq!(preedit_text(&skk), "▼再");
    }

    #[test]
    fn a_bare_angle_bracket_passes_through() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // 変換していないときの > はただの文字
        assert_eq!(typed(&mut skk, ">"), ">");
    }

    #[test]
    fn hankaku_katakana_mode() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        // C-q で半角カタカナモードへ
        assert!(skk.handle(Key::Ctrl(0x11)).mode_changed);
        assert_eq!(skk.mode, Mode::HankakuKatakana);
        assert_eq!(typed(&mut skk, "nihongo"), "ﾆﾎﾝｺﾞ");
        // もう一度でひらがなへ戻る
        skk.handle(Key::Ctrl(0x11));
        assert_eq!(skk.mode, Mode::Hiragana);
        assert_eq!(typed(&mut skk, "aiu"), "あいう");
    }

    #[test]
    fn hankaku_katakana_from_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Nihongo");
        assert_eq!(preedit_text(&skk), "▽にほんご");
        // ▽ の途中の C-q は見出し語を半角カタカナにして確定する
        let r = skk.handle(Key::Ctrl(0x11));
        assert_eq!(String::from_utf8(r.to_child()).unwrap(), "ﾆﾎﾝｺﾞ");
        assert!(preedit_text(&skk).is_empty());
    }

    #[test]
    fn hankaku_katakana_shows_in_the_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        skk.handle(Key::Ctrl(0x11));
        typed(&mut skk, "Nihongo");
        // 見出し語もモードに合わせて出る
        assert_eq!(preedit_text(&skk), "▽ﾆﾎﾝｺﾞ");
    }

    #[test]
    fn float_layout_moves_the_list_off_the_line() {
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/挨/曖/哀/")]);
        skk.set_config(Config::parse("[candidates]\nlayout = \"float\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        let p = skk.preedit();
        // 行内は ▼ とモードの印だけ。一覧は浮かせる側へ。
        assert_eq!(
            p.at_cursor
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "▼挨"
        );
        assert!(p.floating.iter().any(|s| s.text.contains("挨")));
        assert!(!p.floating.is_empty());
    }

    #[test]
    fn inline_layout_keeps_everything_on_the_line() {
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/挨/曖/哀/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        let p = skk.preedit();
        assert!(p.floating.is_empty());
        assert!(p.at_cursor.iter().any(|s| s.text.contains("挨")));
    }

    #[test]
    fn shows_how_many_candidates_remain() {
        // 一覧に載りきらない分は件数で知らせる (ddskk と同じ)
        let mut skk = skk_with(&[("あい", "/1/2/3/4/5/6/7/8/9/10/11/12/13/14/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Ai     ");
        assert!(preedit_text(&skk).contains("[残り 3]"));
    }

    #[test]
    fn cell_marker_tints_the_cursor_without_adding_anything() {
        let mut skk = skk_with(&[]);
        // 既定はセルに色を敷く方式。文字は足さない。
        assert_eq!(skk.marker(), Marker::Cell);
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        assert!(skk.preedit().is_empty());

        skk.handle(Key::Ctrl(0x0a));
        let p = skk.preedit();
        assert_eq!(
            p.cursor_tint,
            Some(Tint {
                style: Style::ModeHiragana,
                offset: 0,
                glyph: None
            })
        );
        assert!(p.at_cursor.is_empty(), "文字は足さない");
        assert!(!p.is_empty(), "色を敷くので描くものはある");

        skk.handle(Key::Char('q'));
        assert_eq!(
            skk.preedit().cursor_tint.map(|t| t.style),
            Some(Style::ModeKatakana)
        );

        // 打ち込み中はその先頭が同じ場所に来るので敷かない
        skk.handle(Key::Char('q'));
        typed(&mut skk, "Kanji");
        assert!(skk.preedit().cursor_tint.is_none());
        assert_eq!(preedit_text(&skk), "▽かんじ");
    }

    #[test]
    fn beside_marker_sits_next_to_the_cursor() {
        // カーソルに覆われない位置に置く。多重化器がカーソルの見た目を遅れて
        // 同期する環境でも確実に見える。
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"beside\"\n").unwrap());
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(
            skk.preedit().cursor_tint,
            Some(Tint {
                style: Style::ModeHiragana,
                offset: 1,
                glyph: None
            })
        );
        assert!(skk.preedit().at_cursor.is_empty(), "文字は足さない");
    }

    #[test]
    fn symbol_marker_shows_a_halfwidth_letter() {
        // 色に頼らずモードが分かる。幅は 1 桁なので見た目も崩れない。
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"symbol\"\n").unwrap());
        assert!(skk.preedit().cursor_tint.is_none(), "ASCII では何もしない");
        for (key, glyph) in [
            (Key::Ctrl(0x0a), '~'),
            (Key::Char('q'), '+'),
            (Key::Ctrl(0x11), '-'),
        ] {
            skk.handle(key);
            let t = skk.preedit().cursor_tint.expect("印が出る");
            assert_eq!(t.glyph, Some(glyph));
            assert_eq!(t.offset, 0, "カーソルの真上");
        }
        skk.handle(Key::Ctrl(0x11));
        skk.handle(Key::Char('L'));
        assert_eq!(skk.preedit().cursor_tint.unwrap().glyph, Some('@'));

        // 記号は設定で変えられる。半角一桁だけを認める。
        skk.set_config(
            Config::parse(
                "[behavior]\nmode_marker = \"symbol\"\n[behavior.mode_symbols]\nhiragana = \"#\"\n",
            )
            .unwrap(),
        );
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(skk.preedit().cursor_tint.unwrap().glyph, Some('#'));
        assert!(Config::parse("[behavior.mode_symbols]\nhiragana = \"あ\"\n").is_err());
        assert!(Config::parse("[behavior.mode_symbols]\nhiragana = \"ab\"\n").is_err());
        // 知らない名前は読み飛ばされる (誤りにするのは `--check-config` の仕事)。
        // 詳しくは config の checking_rejects_what_reading_skips を見ること。
        assert_eq!(
            Config::parse("[behavior.mode_symbols]\nfoo = \"#\"\n").unwrap(),
            Config::default()
        );
    }

    #[test]
    fn letter_marker_appends_a_letter() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"letter\"\n").unwrap());
        assert_eq!(mode_marker(&skk), "", "ASCII では何も描かない");

        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(mode_marker(&skk), "あ");
        skk.handle(Key::Char('q'));
        assert_eq!(mode_marker(&skk), "ア");
        skk.handle(Key::Ctrl(0x11));
        assert_eq!(mode_marker(&skk), "半");
        skk.handle(Key::Ctrl(0x11));
        skk.handle(Key::Char('L'));
        assert_eq!(mode_marker(&skk), "Ａ");

        // 印は打ち込み中の内容より後ろに出る
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        let texts: Vec<String> = skk
            .preedit()
            .at_cursor
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(texts.last().map(|s| s.as_str()), Some("あ"));
        assert!(skk.preedit().cursor_tint.is_none());
    }

    #[test]
    fn mode_marker_can_be_turned_off() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[behavior]\nmode_marker = \"off\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(mode_marker(&skk), "");
        assert!(skk.preedit().cursor_tint.is_none());
        assert!(
            skk.preedit().is_empty(),
            "何も打っていなければ描くものが無い"
        );
    }

    #[test]
    fn custom_bindings_are_honoured() {
        let cfg = Config::parse(
            r#"
            [keys]
            kana = "C-o"
            ascii = "@"
            katakana = "~"
            convert = "C-space"
            previous = "-"
            select = ["1", "2"]

            [candidates]
            inline = 1
            "#,
        )
        .unwrap();
        let mut skk = skk_with(&[("あい", "/愛/藍/相/合/"), ("かんじ", "/漢字/")]);
        skk.set_config(cfg);

        // C-j はもう効かない (素のまま子へ行く)
        assert_eq!(skk.handle(Key::Ctrl(0x0a)).to_child(), vec![0x0a]);
        // C-o でかなモードへ
        assert!(skk.handle(Key::Ctrl(0x0f)).mode_changed);
        assert_eq!(typed(&mut skk, "ai"), "あい");
        // ~ でカタカナ、@ で ASCII
        skk.handle(Key::Char('~'));
        assert_eq!(skk.mode, Mode::Katakana);
        skk.handle(Key::Char('@'));
        assert_eq!(skk.mode, Mode::Ascii);

        // 変換は C-space、一覧は inline = 1 なので 2 番目から出て 1 2 で選ぶ
        skk.handle(Key::Ctrl(0x0f));
        typed(&mut skk, "Ai");
        skk.handle(Key::Ctrl(0x00));
        assert_eq!(preedit_text(&skk), "▼愛");
        skk.handle(Key::Ctrl(0x00));
        assert!(preedit_text(&skk).contains("1:藍"));
        assert!(preedit_text(&skk).contains("2:相"));
        assert_eq!(skk.handle(Key::Char('2')).to_child(), "相".as_bytes());
    }

    #[test]
    fn reconfiguring_mid_conversion_keeps_the_state() {
        let mut skk = skk_with(&[("かんじ", "/漢字/感じ/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        // 変換の途中で設定が差し替わっても見出し語と候補は残る
        let cfg = Config::parse("[keys]\nconvert = \"C-n\"\n").unwrap();
        skk.set_config(cfg);
        assert_eq!(preedit_text(&skk), "▼漢字");
        skk.handle(Key::Ctrl(0x0e));
        assert_eq!(preedit_text(&skk), "▼感じ");
        // 古い割り当ての space は候補を確定してから子へ流れる
        assert_eq!(typed(&mut skk, " "), "感じ ");
    }

    #[test]
    fn enter_confirms_regardless_of_bindings() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.set_config(Config::parse("[keys]\nconfirm = \"C-m\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        // 変換中の Enter は改行を送らずに確定する
        let r = skk.handle(Key::Enter);
        assert_eq!(String::from_utf8(r.to_child()).unwrap(), "漢字");
        // 直接入力に戻れば Enter は素通し (コマンドの実行を邪魔しない)
        assert_eq!(skk.handle(Key::Enter).to_child(), vec![0x0d]);
    }

    #[test]
    fn ascii_passes_through() {
        let mut skk = skk_with(&[]);
        assert_eq!(typed(&mut skk, "ls -la"), "ls -la");
    }

    #[test]
    fn kana_mode_emits_kana() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "aiueo"), "あいうえお");
    }

    #[test]
    fn pending_romaji_is_not_sent() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "k"), "");
        assert_eq!(preedit_text(&skk), "k");
        assert_eq!(typed(&mut skk, "a"), "か");
        assert_eq!(preedit_text(&skk), "");
    }

    #[test]
    fn conversion_without_okuri() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "Kanji"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        typed(&mut skk, " ");
        assert_eq!(preedit_text(&skk), "▼幹事");
        assert_eq!(typed(&mut skk, "\n"), "幹事");
    }

    #[test]
    fn conversion_with_okuri() {
        let mut skk = skk_with(&[("うごk", "/動/")]);
        skk.handle(Key::Ctrl(0x0a));
        // UgoKu → 「動く」
        typed(&mut skk, "Ugo");
        assert_eq!(preedit_text(&skk), "▽うご");
        typed(&mut skk, "Ku");
        assert_eq!(preedit_text(&skk), "▼動く");
        assert_eq!(typed(&mut skk, "\n"), "動く");
    }

    /// 「多く」と「大きい」を一度ずつ直してから、互いを引きずらないことを見る。
    ///
    /// 送り仮名は一文字目で変換に入るので、`OoKu` は送り仮名「く」、`OoKi` は「き」。
    /// どちらも見出し語は `おおk` で、見出し語だけで覚えると混ざる。
    fn learn_both_readings(skk: &mut Skk) {
        // 「多く」を選んで確定する。見出し語 おおk の先頭は「多」になる。
        typed(skk, "OoKu");
        assert_eq!(preedit_text(skk), "▼大く", "辞書の順では「大」が先");
        typed(skk, " ");
        assert_eq!(preedit_text(skk), "▼多く");
        assert_eq!(typed(skk, "\n"), "多く");

        // 続けて「大きい」。**ここはまだ引きずられる** — 「き」の宛先がまだ無い。
        typed(skk, "OoKi");
        assert_eq!(preedit_text(skk), "▼多き", "見出し語の学習が効いている");
        typed(skk, " ");
        assert_eq!(preedit_text(skk), "▼大き");
        assert_eq!(typed(skk, "i"), "大きい");
    }

    #[test]
    fn learning_is_kept_apart_by_okurigana() {
        let mut skk = skk_with(&[("おおk", "/大/多/")]);
        skk.handle(Key::Ctrl(0x0a));
        learn_both_readings(&mut skk);

        // **ここからが本題。** 見出し語の先頭は「大」になったが、送り仮名「く」の
        // 宛先は「多」なので、「おおく」は引きずられない。
        typed(&mut skk, "OoKu");
        assert_eq!(preedit_text(&skk), "▼多く");
        typed(&mut skk, "\x07\x07");
        // 「き」の側も自分の宛先が出る
        typed(&mut skk, "OoKi");
        assert_eq!(preedit_text(&skk), "▼大き");
    }

    /// `off` にすると ddskk の既定と同じ。見出し語の学習がそのまま出る。
    #[test]
    fn okurigana_matching_can_be_turned_off() {
        let mut skk = skk_with(&[("おおk", "/大/多/")]);
        skk.set_config(Config::parse("[behavior]\nokuri_match = \"off\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        learn_both_readings(&mut skk);

        // 送り仮名を見ないので、直前に確定した「大」が「おおく」にも出る
        typed(&mut skk, "OoKu");
        assert_eq!(preedit_text(&skk), "▼大く");
    }

    /// 画面の文脈で同音異義語の順序が変わる。
    #[test]
    fn context_reorders_the_homophones() {
        let entries = [(
            "こうせい",
            "/構成/公正;fair/校正;proofread.「新聞の-」/厚生;welfare.「-労働省」/",
        )];
        let convert = |screen: &str, on: bool| {
            let mut skk = skk_with(&entries);
            let toml = format!("[behavior]\ncontext_order = {on}\n");
            skk.set_config(Config::parse(&toml).unwrap());
            skk.set_context(screen, screen.chars().count());
            skk.handle(Key::Ctrl(0x0a));
            typed(&mut skk, "Kousei ");
            // 注釈は別の話なので落として、選ばれた候補だけを見る
            let shown = preedit_text(&skk);
            shown.split(" ; ").next().unwrap_or("").to_string()
        };

        // 何も無ければ辞書の順
        assert_eq!(convert("", true), "▼構成");
        // 画面にその語があれば上がる (第一段)
        assert_eq!(convert("校正の指示をまとめる。", true), "▼校正");
        // 注釈の訳語でも上がる (第二段)
        assert_eq!(convert("please proofread the draft", true), "▼校正");
        // 注釈の用例語でも上がる
        assert_eq!(convert("労働省の発表を読む", true), "▼厚生");
        // 関わりの無い画面では動かない
        assert_eq!(convert("$ cargo test --quiet", true), "▼構成");
        // 無効なら何があっても動かない
        assert_eq!(convert("校正の指示をまとめる。", false), "▼構成");
    }

    /// 送り仮名と食い違ったら送り仮名を優先する。語を決める手掛かりだから。
    #[test]
    fn okurigana_wins_over_the_context() {
        let mut skk = skk_with(&[("おおk", "/大/多/")]);
        skk.set_config(Config::parse("[behavior]\ncontext_order = true\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        learn_both_readings(&mut skk);

        // 画面は「大」だらけだが、送り仮名「く」の宛先は「多」
        skk.set_context("大きい大きい大きい", 9);
        typed(&mut skk, "OoKu");
        assert_eq!(preedit_text(&skk), "▼多く");
    }

    /// 宛先が貯まっていない見出し語では並びが変わらない。入れて悪くならないこと。
    #[test]
    fn okurigana_order_leaves_unknown_readings_alone() {
        let mut skk = skk_with(&[("おおk", "/大/多/"), ("うごk", "/動/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "OoKi");
        assert_eq!(preedit_text(&skk), "▼大き", "辞書の順のまま");
        typed(&mut skk, "\x07\x07");
        typed(&mut skk, "UgoKu");
        assert_eq!(preedit_text(&skk), "▼動く");
    }

    #[test]
    fn learning_moves_candidate_to_front() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji  \n");
        // 二回目は学習した「幹事」が先頭に出る
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼幹事");
    }

    #[test]
    fn cancel_discards_everything() {
        let mut skk = skk_with(&[("かんじ", "/漢字/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        assert_eq!(typed(&mut skk, "\x07"), "");
        assert_eq!(preedit_text(&skk), "");
    }

    /// C-g で ▼ から ▽ に戻したあと、見出し語の途中を直して変換し直せる。
    #[test]
    fn the_reading_can_be_edited_after_cancelling_a_conversion() {
        let mut skk = skk_with(&[("かんじ", "/漢字/"), ("かじ", "/家事/火事/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji ");
        assert_eq!(preedit_text(&skk), "▼漢字");

        // C-g で ▽ へ戻る。カーソルは末尾にあるので、区間はまだ分かれない。
        typed(&mut skk, "\x07");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        assert_eq!(cursor_char(&skk), "");

        // C-b で一文字ずつ戻る
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "じ");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "ん");

        // C-d でその一文字だけ消し、そのまま変換し直す
        skk.handle(Key::Ctrl(0x04));
        assert_eq!(preedit_text(&skk), "▽かじ");
        assert_eq!(cursor_char(&skk), "じ");
        assert_eq!(typed(&mut skk, " "), "");
        assert_eq!(preedit_text(&skk), "▼家事");
        assert_eq!(typed(&mut skk, "\n"), "家事");
    }

    /// 打ち足すのはカーソルの位置。
    #[test]
    fn typing_goes_where_the_cursor_is() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kaji");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "じ");
        typed(&mut skk, "nn");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        assert_eq!(cursor_char(&skk), "じ", "カーソルは打った文字の後ろに残る");
        assert_eq!(typed(&mut skk, "\n"), "かんじ");
    }

    /// Backspace はカーソルの手前を消す。末尾にいる間はこれまでどおり。
    #[test]
    fn backspace_follows_the_cursor() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽かん", "末尾では末尾から消える");

        typed(&mut skk, "ji");
        skk.handle(Key::Ctrl(0x02));
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "ん");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽んじ");
        assert_eq!(cursor_char(&skk), "ん");
    }

    /// カーソルは見出し語の両端で止まる。先頭での Backspace は取り消しにならない。
    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");

        skk.handle(Key::Ctrl(0x01)); // C-a
        assert_eq!(cursor_char(&skk), "か");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "か", "先頭より左へは行かない");
        typed(&mut skk, "\x7f");
        assert_eq!(
            preedit_text(&skk),
            "▽かんじ",
            "消すものが無いだけで ▽ は残る"
        );

        skk.handle(Key::Ctrl(0x05)); // C-e
        assert_eq!(cursor_char(&skk), "", "末尾では区間が分かれない");
        skk.handle(Key::Ctrl(0x06));
        assert_eq!(cursor_char(&skk), "");
        skk.handle(Key::Ctrl(0x04));
        assert_eq!(preedit_text(&skk), "▽かんじ", "末尾の C-d は何もしない");
    }

    /// 矢印でも動く。端末が送る形が変わっても同じ。
    #[test]
    fn arrows_move_the_cursor_too() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        skk.handle(Key::Raw(b"\x1b[D".to_vec()));
        assert_eq!(cursor_char(&skk), "じ");
        // アプリケーションカーソルキーモードの形
        skk.handle(Key::Raw(b"\x1bOD".to_vec()));
        assert_eq!(cursor_char(&skk), "ん");
        skk.handle(Key::Raw(b"\x1b[C".to_vec()));
        assert_eq!(cursor_char(&skk), "じ");
    }

    /// 打ちかけのローマ字は、動く前に始末される (確定するときと同じ扱い)。
    #[test]
    fn moving_settles_the_pending_romaji() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanjik");
        assert_eq!(preedit_text(&skk), "▽かんじk");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(preedit_text(&skk), "▽かんじ", "単独の子音は捨てられる");
        assert_eq!(cursor_char(&skk), "じ");

        // n だけは「ん」になってから見出し語に残る
        skk.handle(Key::Ctrl(0x05));
        typed(&mut skk, "n");
        assert_eq!(preedit_text(&skk), "▽かんじn");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(preedit_text(&skk), "▽かんじん");
        assert_eq!(cursor_char(&skk), "ん");
    }

    /// 送りありでも、動かせるのは見出し語の中だけ。
    #[test]
    fn the_cursor_stays_inside_the_reading_when_there_is_okuri() {
        let mut skk = skk_with(&[("うごk", "/動/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "UgoKu");
        assert_eq!(preedit_text(&skk), "▼動く");
        typed(&mut skk, "\x07");
        assert_eq!(preedit_text(&skk), "▽うご*く");

        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "ご");
        assert_eq!(preedit_text(&skk), "▽うご*く", "送り仮名は末尾に残る");
        skk.handle(Key::Ctrl(0x06));
        skk.handle(Key::Ctrl(0x06));
        assert_eq!(cursor_char(&skk), "", "送り仮名の中へは入らない");
        // 末尾へ戻れば、Backspace はこれまでどおり送り仮名から削る
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽うご*");
    }

    /// カタカナモードでも、カーソルの一文字はモードに合わせて出る。
    #[test]
    fn the_cursor_segment_follows_the_mode() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "q");
        typed(&mut skk, "Kanji");
        assert_eq!(preedit_text(&skk), "▽カンジ");
        skk.handle(Key::Ctrl(0x02));
        assert_eq!(cursor_char(&skk), "ジ");
        assert_eq!(preedit_text(&skk), "▽カンジ");
    }

    /// C-h も一文字消すキー。Backspace 鍵と同じ道を通る。
    ///
    /// 端末では `0x08` で届き、拡張鍵盤プロトコルの下では `CSI 104;5u` になる。
    /// どちらも `Key::Ctrl(0x08)` に落ちるので、ここが効けば両方効く。
    #[test]
    fn ctrl_h_deletes_one_character() {
        let mut skk = skk_with(&[("かんじ", "/漢字/幹事/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        assert_eq!(preedit_text(&skk), "▽かんじ");
        skk.handle(Key::Ctrl(0x08));
        assert_eq!(preedit_text(&skk), "▽かん", "▽ の見出し語から一文字消える");

        // ▼ では前の候補へ戻る (Backspace 鍵と同じ)
        typed(&mut skk, "ji ");
        assert_eq!(preedit_text(&skk), "▼漢字");
        skk.handle(Key::Char(' '));
        assert_eq!(preedit_text(&skk), "▼幹事");
        skk.handle(Key::Ctrl(0x08));
        assert_eq!(preedit_text(&skk), "▼漢字");
    }

    /// **消すものが無ければ、押されたキーのまま子へ渡す。**
    ///
    /// `0x7f` にすり替えてはいけない。`C-h` に別の働きを割り当てているアプリで
    /// それが効かなくなる (nvim の窓の移動が実際にそうだった)。文字を打つ段では
    /// アプリ側が `C-h` を手前の一文字消しに割り当てているので、そのまま渡せば消える。
    #[test]
    fn ctrl_h_reaches_the_child_as_itself() {
        let mut skk = skk_with(&[]);
        let cfg = Config::default();
        // かなモードと、素通しの段 (ASCII / 全角英数) のどれでも同じ
        for enter in [None, Some(&cfg.ascii), Some(&cfg.zenkaku)] {
            skk.handle(cfg.kana[0].clone());
            if let Some(e) = enter {
                skk.handle(e[0].clone());
            }
            let r = skk.handle(Key::Ctrl(0x08));
            assert_eq!(r.passthrough, Some(Key::Ctrl(0x08)));
            assert_eq!(r.to_child(), vec![0x08], "{enter:?} の段");
        }
        // Backspace 鍵はこれまでどおり 0x7f のまま
        skk.handle(cfg.kana[0].clone());
        assert_eq!(skk.handle(Key::Backspace).to_child(), vec![0x7f]);
    }

    /// 割り当てから外せば、C-h は子アプリの持ち物に戻る。
    #[test]
    fn ctrl_h_can_be_given_back_to_the_child() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[keys]\nbackspace = \"bs\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        let r = skk.handle(Key::Ctrl(0x08));
        assert_eq!(r.to_child(), vec![0x08], "押されたとおり素通しする");
    }

    /// 移動キーは設定から取る。割り当てを変えれば別のキーで動く。
    #[test]
    fn the_cursor_keys_come_from_the_config() {
        let mut skk = skk_with(&[]);
        skk.set_config(Config::parse("[keys]\nmove_left = \"C-h\"\n").unwrap());
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        skk.handle(Key::Ctrl(0x08));
        assert_eq!(cursor_char(&skk), "じ", "割り当てた C-h で動く");
        // 割り当てを外した C-b は、これまでどおり見出し語を確定して素通しする
        let r = skk.handle(Key::Ctrl(0x02));
        assert_eq!(r.commit, "かんじ");
        assert_eq!(r.passthrough, Some(Key::Ctrl(0x02)));
    }

    #[test]
    fn katakana_toggle() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "q");
        assert_eq!(typed(&mut skk, "aiu"), "アイウ");
    }

    #[test]
    fn q_in_composing_makes_katakana() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        assert_eq!(typed(&mut skk, "Aiuq"), "アイウ");
    }

    #[test]
    fn candidate_list_appears_after_four() {
        let mut skk = skk_with(&[("あ", "/亜/唖/娃/阿/哀/愛/挨/姶/逢/葵/")]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "A     ");
        let text = preedit_text(&skk);
        // 5 番目以降が一覧になる。a=哀 s=愛 d=挨 …
        assert!(text.contains("a:哀"), "一覧が出ていない: {text}");
        assert!(text.contains("d:挨"), "一覧が短い: {text}");
        assert_eq!(typed(&mut skk, "d"), "挨");
    }

    #[test]
    fn backspace_walks_back_through_reading() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        typed(&mut skk, "\x7f");
        assert_eq!(preedit_text(&skk), "▽かん");
    }

    /// herdr の prefix (C-z) が横取りされずに子へ届くこと。作った動機そのもの。
    #[test]
    fn control_keys_reach_the_child() {
        let mut skk = skk_with(&[]);
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child(), vec![0x1a]);
        skk.handle(Key::Ctrl(0x0a)); // かなモードへ
        assert_eq!(skk.handle(Key::Ctrl(0x1a)).to_child(), vec![0x1a]);
        // 入力途中のローマ字があっても、確定させたうえで届く
        skk.handle(Key::Char('k'));
        let r = skk.handle(Key::Ctrl(0x1a));
        assert_eq!(r.to_child(), vec![0x1a]);
    }

    /// 変換中に矢印キーなどが来たら、見出し語を確定してから素通しする。
    #[test]
    fn escape_sequence_confirms_then_passes() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "Kanji");
        let r = skk.handle(Key::Raw(b"\x1b[A".to_vec()));
        assert_eq!(r.to_child(), "かんじ\x1b[A".as_bytes());
        assert_eq!(preedit_text(&skk), "");
    }

    #[test]
    fn zenkaku_mode() {
        let mut skk = skk_with(&[]);
        skk.handle(Key::Ctrl(0x0a));
        typed(&mut skk, "L");
        assert_eq!(typed(&mut skk, "abc"), "ａｂｃ");
    }
}
