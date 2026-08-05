//! 画面に見えている文章を文脈として、同音異義語の順序を決める。
//!
//! **日本語は同音異義語が多すぎる。** 「こうせい」は `SKK-JISYO.L` に 20 件を超える
//! 候補があり (構成・公正・校正・攻勢・後世・更生・厚生・恒星…)、見出し語だけでは
//! どれが欲しいのか決めようがない。最近使った順に並べても、話題が変われば外れる。
//!
//! そこで**画面に見えている文章**を手掛かりにする。ttyskk は子アプリの画面の控えを
//! 持っているので、**子が書いた文字まで文脈にできる** — 他の SKK 実装に無い立場に
//! ある。画面はペインごとなので、話題の切れ目が自然に入るのも都合がよい。
//!
//! # 何を手掛かりにするか
//!
//! 1. **候補そのものが画面にあるか。** 文章は同じ語を繰り返す。「講演」の話をして
//!    いる画面には既に「講演」が出ている
//! 2. **注釈の用例語が画面にあるか。** `SKK-JISYO.L` は `校正;proofread.「新聞の-」`
//!    のように**共起語を注釈に持っている**。画面に `proofread` や「新聞」があれば
//!    校正が上がる。辞書に既にある情報なので、余分なファイルが要らない
//!
//! # 距離の重み
//!
//! カーソルから遠いほど軽くする。**冪関数を使う** — 指数関数は遠くを殺しすぎる。
//! 文書の冒頭で一度だけ「校正」と書いてあり以後ずっとその話、というのは普通に
//! 起こるが、指数だとその手掛かりが消える (λ=200 で 2600 文字先は 2×10⁻⁶)。
//! 自然言語の語の再出現は裾の重い分布に従うので、冪の方が実態に合う。
//!
//! ```text
//! w(d) = 1 / (1 + d/λ)      λ = 200 文字 (重みが半分になる距離)
//!
//!   同じ行 (40)   0.83      20 行上 (800)    0.20
//!   5 行上 (200)  0.50      画面の端 (2600)  0.07
//! ```
//!
//! 同じ語が何度も出てくれば足し合わせる。「近くにある」と「よく出てくる」が混ざる。

/// 重みが半分になる距離 (文字数) の既定値。
///
/// **行数ではなく文字数で数える。** 一行の文字数はフォントの大きさや多重化器の分割で
/// 変わるので、行を単位にすると環境ごとに効き方が変わってしまう。
pub const DEFAULT_HALF_WEIGHT: usize = 200;

/// 手掛かりにしない英単語。
///
/// **注釈の訳語をそのまま拾うと、機能語まで語彙になる。** `光炎;light and fire` から
/// `and` を取ると、英文がある画面ではどこでも当たってしまい、無関係な候補が上がる
/// (実測で「こうえん」が光炎になった)。
///
/// 長さで切ると `law`・`art`・`sea` のような**短い訳語**まで落ちるので、語を名指しする。
const STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "that", "this", "from", "into", "than", "then", "them", "they",
    "there", "here", "have", "has", "had", "was", "were", "been", "being", "are", "not", "but",
    "its", "his", "her", "our", "your", "their", "who", "whom", "which", "what", "when", "where",
    "while", "such", "some", "any", "all", "one", "two", "out", "off", "over", "under", "after",
    "before", "same", "other", "more", "most", "only", "also", "very", "much", "many", "each",
    "both", "can", "could", "will", "would", "shall", "should", "may", "might", "must", "let",
    "use", "used", "using", "make", "made", "get", "got", "see", "say", "said", "thing", "things",
];

/// 注釈から来た手掛かりの重み。候補そのものより弱く見る。
///
/// 「画面にその語がある」は直接の証拠だが、「注釈に書かれた共起語がある」は間接で、
/// 辞書の書き手の主観も混ざる。同じ重みにすると注釈の豊かな候補ばかり上がる。
const HINT_WEIGHT: f64 = 0.5;

/// 画面に見えている文章。
pub struct Context {
    chars: Vec<char>,
    /// カーソルの位置 (`chars` の添字)。
    cursor: usize,
    /// 重みが半分になる距離 (文字数)。
    half: f64,
}

impl Context {
    /// 画面の文字とカーソル位置から作る。既定の重みで見る。
    ///
    /// 文字列は行を繋いだもの。**端末の側で組んで渡す** — エンジンは画面を知らない
    /// ので、GUI の入力メソッドからは周辺テキストを同じ口に流せばよい。
    pub fn new(text: &str, cursor: usize) -> Self {
        Self::with_half_distance(text, cursor, DEFAULT_HALF_WEIGHT)
    }

    /// 重みが半分になる距離を指定して作る。
    ///
    /// 小さくすると近くだけを見る (話題が細かく切り替わる文書向き)。大きくすると
    /// 画面全体を平らに見る (一つの話題が続く長文向き)。0 は指定できない。
    pub fn with_half_distance(text: &str, cursor: usize, half: usize) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let cursor = cursor.min(chars.len());
        Context {
            chars,
            cursor,
            half: half.max(1) as f64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// 記録に残す要約。**何が見えていたのかが分からないと、点数の理由を追えない。**
    ///
    /// 画面が巻き上がって手掛かりが視野の外に出ていた、という場面が実際にあった。
    /// 全体を残すと長すぎるので、大きさとカーソルの前後だけを出す。
    pub fn digest(&self, around: usize) -> String {
        let lo = self.cursor.saturating_sub(around);
        let hi = (self.cursor + around).min(self.chars.len());
        let cut = |r: std::ops::Range<usize>| {
            self.chars[r]
                .iter()
                .map(|c| if *c == '\n' { '⏎' } else { *c })
                .collect::<String>()
        };
        format!(
            "{}文字 位置{} …{}▮{}…",
            self.chars.len(),
            self.cursor,
            cut(lo..self.cursor),
            cut(self.cursor..hi)
        )
    }

    /// 候補ひとつの点数。手掛かりが無ければ 0。
    pub fn score(&self, text: &str, annotation: Option<&str>) -> f64 {
        self.explain(text, annotation).0
    }

    /// 点数と、**効いた語**。狙いと違う候補が出た理由は、これが無いと追えない。
    ///
    /// 返す語は重い順。点の付かなかった語は含まない。
    pub fn explain(&self, text: &str, annotation: Option<&str>) -> (f64, Vec<(String, f64)>) {
        let mut hits: Vec<(String, f64)> = Vec::new();
        let w = self.weight_of(text);
        if w > 0.0 {
            hits.push((text.to_string(), w));
        }
        if let Some(a) = annotation {
            for hint in hints(a) {
                let w = HINT_WEIGHT * self.weight_of(&hint);
                if w > 0.0 {
                    hits.push((hint, w));
                }
            }
        }
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        (hits.iter().map(|(_, w)| w).sum(), hits)
    }

    /// その語が画面に出てくる分の重みの合計。
    fn weight_of(&self, needle: &str) -> f64 {
        let n: Vec<char> = needle.chars().collect();
        if n.is_empty() || n.len() > self.chars.len() {
            return 0.0;
        }
        // 英字の語は大文字小文字を無視して照合する
        let ascii = n.iter().all(|c| c.is_ascii_alphabetic());
        let mut total = 0.0;
        for at in 0..=self.chars.len() - n.len() {
            // **まず一文字目だけ見る。** 候補と注釈の手掛かりで一回の変換に 60 回
            // ほど画面を舐めるので、外れる位置を安く捨てられるかどうかが効く。
            let head = self.chars[at];
            if if ascii {
                !head.eq_ignore_ascii_case(&n[0])
            } else {
                head != n[0]
            } {
                continue;
            }
            let hit = if ascii {
                self.chars[at..at + n.len()]
                    .iter()
                    .zip(&n)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
            } else {
                self.chars[at..at + n.len()] == n[..]
            };
            if hit && self.boundary_ok(at, n.len(), ascii) {
                total += 1.0 / (1.0 + self.distance(at, n.len()) / self.half);
            }
        }
        total
    }

    /// その出方を数えてよいか。
    ///
    /// **一文字の候補は、同じ字種の連なりがちょうどその一文字であること**を求める。
    /// 「公」が「公園」「公演」の中に当たってしまうと信号にならない。漢字以外の文字を
    /// 区切りとみなせば、「公とする」だけを拾える。一文字の変換は実際よく使うので、
    /// まとめて対象外にはしない。
    ///
    /// 英字は語の境界で見る。**識別子やパスの一部は数えない** — 端末の画面は
    /// `src/main.rs` や `--config-example` で埋まっていて、人の文章ではない。
    fn boundary_ok(&self, at: usize, len: usize, ascii: bool) -> bool {
        let before = at.checked_sub(1).map(|i| self.chars[i]);
        let after = self.chars.get(at + len).copied();
        if ascii {
            let joined = |c: Option<char>| {
                c.is_some_and(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
            };
            return !joined(before) && !joined(after);
        }
        if len > 1 {
            return true;
        }
        let class = class_of(self.chars[at]);
        let same = |c: Option<char>| c.is_some_and(|c| class_of(c) == class);
        !same(before) && !same(after)
    }

    /// カーソルからの距離 (文字数)。範囲に入っていれば 0。
    fn distance(&self, at: usize, len: usize) -> f64 {
        if self.cursor < at {
            (at - self.cursor) as f64
        } else if self.cursor >= at + len {
            (self.cursor - (at + len - 1)) as f64
        } else {
            0.0
        }
    }
}

/// 文字の種類。一文字の候補の区切りを見るために使う。
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Kanji,
    Kana,
    Ascii,
    Other,
}

fn class_of(c: char) -> Class {
    match c {
        '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{fadf}' => Class::Kanji,
        '\u{3041}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{ff66}'..='\u{ff9d}' => Class::Kana,
        c if c.is_ascii_alphanumeric() => Class::Ascii,
        _ => Class::Other,
    }
}

/// 注釈から共起語を取り出す。
///
/// `SKK-JISYO.L` の注釈は二通りの手掛かりを持っている。
///
/// ```text
/// 校正;proofread.「新聞の-」   → proofread, 新聞
/// 厚生;welfare.「-労働省」      → welfare, 労働省
/// 抗生;antibiotic.「-物質」     → antibiotic, 物質
/// 夛;「多」の異体字             → (無し)
/// ```
///
/// **鉤括弧の中の漢字の連なりだけ**を取る。括弧の外まで拾うと、`夛;「多」の異体字`
/// の「異体字」のような**語の説明**まで共起語になってしまう。一文字の連なりも捨てる —
/// 上の「多」は候補そのものを指しており、これを手掛かりにすると「多」が画面にある
/// だけで異体字が上がる。
///
/// 英字は注釈のどこにあっても取る。訳語はほぼ例外なく共起語として働く。
fn hints(annotation: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut run = String::new();
    let mut quoted = false;

    let flush_word = |w: &mut String, out: &mut Vec<String>| {
        let lower = w.to_lowercase();
        if lower.chars().count() >= 3 && !STOPWORDS.contains(&lower.as_str()) {
            out.push(lower);
        }
        w.clear();
    };
    let flush_run = |r: &mut String, out: &mut Vec<String>| {
        if r.chars().count() >= 2 {
            out.push(std::mem::take(r));
        } else {
            r.clear();
        }
    };

    for c in annotation.chars() {
        match c {
            '「' => {
                quoted = true;
                flush_run(&mut run, &mut out);
            }
            '」' => {
                quoted = false;
                flush_run(&mut run, &mut out);
            }
            c if c.is_ascii_alphabetic() => {
                flush_run(&mut run, &mut out);
                word.push(c);
                continue;
            }
            c if quoted && class_of(c) == Class::Kanji => {
                flush_word(&mut word, &mut out);
                run.push(c);
                continue;
            }
            _ => flush_run(&mut run, &mut out),
        }
        flush_word(&mut word, &mut out);
    }
    flush_word(&mut word, &mut out);
    flush_run(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 点数そのものではなく、**並べ替えた結果**で見る。重みの定数を動かしても
    /// 壊れないし、利用者に見えるのはこちらだから。
    fn order(screen: &str, cursor: usize, cands: &[(&str, Option<&str>)]) -> Vec<String> {
        let ctx = Context::new(screen, cursor);
        let mut v: Vec<(f64, usize, &str)> = cands
            .iter()
            .enumerate()
            .map(|(i, (t, a))| (ctx.score(t, *a), i, *t))
            .collect();
        // 点数の降順、同点なら元の順 (安定)
        v.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, _, t)| t.to_string()).collect()
    }

    /// 候補そのものが画面にあれば上がる (第一段)。
    #[test]
    fn a_candidate_already_on_screen_comes_first() {
        let screen = "きょうの構成をなおす。校正はあとまわし。";
        let cands = [("構成", None), ("公正", None), ("校正", None)];
        // 「構成」がカーソルに近いので先頭
        assert_eq!(order(screen, 8, &cands)[0], "構成");
        // カーソルを「校正」側へ寄せると入れ替わる
        assert_eq!(order(screen, 20, &cands)[0], "校正");
    }

    /// 手掛かりが無ければ並びを変えない。入れて悪くならないこと。
    #[test]
    fn an_unrelated_screen_leaves_the_order_alone() {
        let screen = "$ cargo test --quiet\n   Compiling ttyskk v0.1.1\n";
        let cands = [("構成", None), ("公正", None), ("校正", None)];
        assert_eq!(order(screen, 10, &cands), ["構成", "公正", "校正"]);
    }

    /// 注釈の用例語と訳語が効く (第二段)。
    #[test]
    fn the_annotation_supplies_the_hints() {
        let cands = [
            ("構成", None),
            ("校正", Some("proofread.「新聞の-」")),
            ("厚生", Some("welfare.「-労働省」")),
        ];
        // 英語の訳語で当たる
        assert_eq!(order("let me proofread this draft", 27, &cands)[0], "校正");
        // 鉤括弧の中の用例語でも当たる
        assert_eq!(order("労働省の発表によると", 10, &cands)[0], "厚生");
        // 手掛かりが画面に無ければ動かない
        assert_eq!(order("まったく別の話題です", 10, &cands)[0], "構成");
    }

    /// 一文字の候補は、漢字の連なりがちょうどその一文字のときだけ当たる。
    #[test]
    fn a_single_kanji_needs_the_whole_run() {
        let cands = [("好", None), ("公", None)];
        // 「公園」の中の「公」は数えない。並びは元のまま。
        assert_eq!(order("公園であそぶ", 6, &cands), ["好", "公"]);
        // 単独で立っていれば数える
        assert_eq!(order("公とする", 4, &cands), ["公", "好"]);
    }

    /// 英字は語の境界で見る。識別子やパスの一部は数えない。
    #[test]
    fn english_matches_only_whole_words() {
        let cands = [("構成", None), ("校正", Some("proofread"))];
        assert_eq!(order("proofread the draft", 19, &cands)[0], "校正");
        // 大文字小文字は無視する
        assert_eq!(order("Proofread the draft", 19, &cands)[0], "校正");
        // 端末の画面はパスや識別子で埋まっている。人の文章ではないので数えない。
        assert_eq!(order("src/proofread_helper.rs", 23, &cands)[0], "構成");
    }

    /// 遠くても消えない。指数関数だとここが 0 になる。
    #[test]
    fn a_distant_mention_still_counts() {
        let far = format!("校正の話。{}いま", "あ".repeat(2000));
        let ctx = Context::new(&far, far.chars().count());
        assert!(ctx.score("校正", None) > 0.0, "遠いだけで消してはいけない");
        // ただし近い方が重い
        let near = Context::new("校正の話。いま", 7);
        assert!(near.score("校正", None) > ctx.score("校正", None));
    }

    /// 半減の距離を変えると、遠い手掛かりの効き方が変わる。
    ///
    /// **距離は文字数で数える。** 一行の文字数はフォントや画面の分割で変わるので、
    /// 行を単位にすると環境ごとに効き方がぶれる。
    #[test]
    fn the_half_distance_changes_how_far_it_reaches() {
        // 200 文字先に手掛かりを一つだけ置く
        let far = format!("校正{}いま", "あ".repeat(200));
        let at = far.chars().count();
        let near_sighted = Context::with_half_distance(&far, at, 50);
        let default = Context::with_half_distance(&far, at, 200);
        let far_sighted = Context::with_half_distance(&far, at, 800);

        // 大きいほど遠くまで効く
        assert!(near_sighted.score("校正", None) < default.score("校正", None));
        assert!(default.score("校正", None) < far_sighted.score("校正", None));
        // 半減の距離ちょうどなら、およそ半分
        let w = default.score("校正", None);
        assert!((0.45..0.55).contains(&w), "200 文字先で半分になるはず: {w}");
        // 0 を渡しても壊れない (1 として扱う)
        assert!(Context::with_half_distance(&far, at, 0).score("校正", None) > 0.0);
    }

    /// 何度も出てくれば重くなる。
    #[test]
    fn repeated_mentions_add_up() {
        let once = Context::new("校正をする", 5);
        let twice = Context::new("校正をする。校正をする", 11);
        assert!(twice.score("校正", None) > once.score("校正", None));
    }

    #[test]
    fn hints_are_taken_from_the_annotation() {
        assert_eq!(hints("proofread.「新聞の-」"), ["proofread", "新聞"]);
        assert_eq!(hints("welfare.「-労働省」"), ["welfare", "労働省"]);
        assert_eq!(hints("†lecture.「作家の-」"), ["lecture", "作家"]);
        // 一文字の連なりは候補そのものを指しているので捨てる
        assert_eq!(hints("「多」の異体字"), Vec::<String>::new());
        // 鉤括弧の外の漢字は語の説明なので取らない
        assert_eq!(hints("[生物]tropism"), ["tropism"]);
        assert_eq!(hints("学校名"), Vec::<String>::new());
        // 短すぎる英字は語として扱わない
        assert_eq!(hints("=紅炎"), Vec::<String>::new());
        // **機能語は手掛かりにしない。** `light and fire` の `and` を拾うと、英文の
        // ある画面ではどこでも当たってしまう (実測で「こうえん」が光炎になった)。
        assert_eq!(hints("light and fire"), ["light", "fire"]);
        assert_eq!(hints("good performance"), ["good", "performance"]);
        // 短くても機能語でなければ残す。訳語には短いものがある。
        assert_eq!(hints("law"), ["law"]);
    }

    /// 機能語が当たらないことを、点数の側からも見る。
    #[test]
    fn function_words_do_not_score() {
        let screen = "There are a few typos and the date looks wrong.";
        let ctx = Context::new(screen, screen.chars().count());
        assert_eq!(ctx.score("光炎", Some("light and fire")), 0.0);
        // 中身のある語なら当たる
        assert!(ctx.score("誤植", Some("typos")) > 0.0);
    }
}
