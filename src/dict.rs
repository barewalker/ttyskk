//! SKK 辞書の読み込みと引き当て。
//!
//! 見出し語は送りなしが「かな」、送りありが「かな + 送り仮名のローマ字頭文字」
//! (例: 「動く」なら `うごk`)。この二つは同じ表に入れても衝突しないため、
//! ひとつの HashMap で扱う。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

/// 同梱する補助辞書。どの標準辞書にも入っていないが、無いと困るものを持つ。
/// いまは丸数字 (①〜㊿) だけ。バイナリに埋め込むので、置き場所の設定が要らない。
const BUILTIN: &str = include_str!("../dict/SKK-JISYO.ttyskk");

/// 送り仮名ごとの宛先。`おおk /大/多/[き/大/]/[く/多/]/` の `[...]` にあたる。
///
/// **送りありの見出し語は、送り仮名が違っても同じ**になる。「大きい」は `OoKii`、
/// 「多く」は `OoKu` で、どちらも `おおk` を引く。候補は語幹だけなので、見出し語
/// 単位で覚えると片方の学習がもう片方を引きずる (「大きい」を確定したあと「おおく」
/// と打つと `▼大く` が先に出る)。送り仮名ごとに宛先を分けると、これが解ける。
///
/// 送り仮名 → 候補の本文の並び。注釈は持たない (SKK の書き方がそうなっている)。
pub type OkuriBlocks = BTreeMap<String, Vec<String>>;

/// 候補ひとつ。`;` 以降の注釈は分けて持つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub annotation: Option<String>,
}

impl Candidate {
    fn parse(s: &str) -> Option<Self> {
        // 空、または半角の空白だけの候補は捨てる。SKK では意味を持たないうえ、
        // 利用者辞書に紛れ込むと共有辞書の正しい候補を覆い隠す (登録に失敗した
        // 跡としてしばしば残っている)。全角空白は正当な候補なので残す。
        if s.trim_matches([' ', '\t']).is_empty() {
            return None;
        }
        match s.split_once(';') {
            Some((text, annot)) if !text.is_empty() => Some(Candidate {
                text: text.to_string(),
                annotation: Some(annot.to_string()),
            }),
            _ => Some(Candidate {
                text: s.to_string(),
                annotation: None,
            }),
        }
    }

    fn serialize(&self) -> String {
        match &self.annotation {
            Some(a) => format!("{};{}", self.text, a),
            None => self.text.clone(),
        }
    }
}

/// 辞書一式。利用者辞書を先に引き、次にスニペット、最後に共有辞書を引く。
pub struct Dict {
    system: HashMap<String, Vec<Candidate>>,
    user: HashMap<String, Vec<Candidate>>,
    /// 送り仮名ごとの宛先。候補の表とは別に持つ。
    ///
    /// 候補の表に混ぜると、引く・数える・並べ替えるすべての場所が送りありの事情を
    /// 抱えることになる。使うのは送りありの変換と保存だけなので、分けておく。
    system_okuri: HashMap<String, OkuriBlocks>,
    user_okuri: HashMap<String, OkuriBlocks>,
    /// 定型文 (`*.code-snippets`)。**手元の編集器で書くもの**で、学習では動かさない。
    ///
    /// 並びはファイルに書いた順がそのまま出る。住所を書き換えたときに古い候補が
    /// 先頭に残らないよう、確定しても利用者辞書へは写さない ([`Dict::learn`])。
    snippets: HashMap<String, Vec<Candidate>>,
    snippet_paths: Vec<PathBuf>,
    /// スニペットを最後に読んだときの状態。編集器で保存されたら読み直す。
    snippet_stamps: Vec<Option<(SystemTime, u64)>>,
    user_path: PathBuf,
    import_path: Option<PathBuf>,
    /// この起動で辞書に加えた変更を、起きた順に記録する。
    ///
    /// 丸ごと書き出すと、herdr で複数のペインを開いているときに互いの学習を
    /// 消し合ってしまう。保存時にディスクの内容を読み直し、この記録だけを
    /// 順に重ねることで、他のペインの学習を残したまま書ける。
    changes: Vec<Change>,
    /// 利用者辞書を最後に読んだ (または書いた) ときの状態。
    ///
    /// 別のプロセスが書き換えたかどうかを、これと突き合わせて判る。自分で書いた
    /// 直後にも更新するので、自分の保存で読み直しが走ることはない。
    user_stamp: Option<(SystemTime, u64)>,
    /// 共有辞書の見出し語を並べ替えたもの。前方一致を二分探索で取り出す。
    ///
    /// 17 万語を毎回舐めると一回 5〜7 ms かかり、打鍵ごとに引く用途には重い。
    /// 送りありの見出し語は補完の対象外なので最初から除いてある。共有辞書は
    /// 読み込み後に変わらないので、索引も一度作れば足りる。
    system_sorted: Vec<String>,
}

/// 利用者辞書への変更。順に適用するので、覚え直しと削除が入り混じっても筋が通る。
#[derive(Clone, Debug)]
enum Change {
    /// 確定した候補を先頭へ移す
    Learn(String, Candidate),
    /// 送り仮名ごとの宛先へ覚える (見出し語, 送り仮名, 候補の本文)
    LearnOkuri(String, String, String),
    /// 候補を取り除く
    Purge(String, String),
}

/// 一行を「見出し語」「候補列」「送り仮名ごとの宛先」に分ける。
///
/// 送り仮名ブロック (`[り/送/]`) は **`/` で素朴に分けると `[り` と `]` に散る**ので、
/// `[` で始まる区間から `]` までを一つのまとまりとして拾う。ddskk 由来の書き方で、
/// CorvusSKK も書く。
fn parse_line(line: &str) -> Option<(String, Vec<Candidate>, OkuriBlocks)> {
    if line.is_empty() || line.starts_with(';') {
        return None;
    }
    let (key, rest) = line.split_once(' ')?;
    let body = rest.trim();
    let body = body.strip_prefix('/').unwrap_or(body);
    let body = body.strip_suffix('/').unwrap_or(body);

    let mut cands: Vec<Candidate> = Vec::new();
    let mut blocks = OkuriBlocks::new();
    let mut open: Option<(String, Vec<String>)> = None;
    for seg in body.split('/') {
        if let Some(okuri) = seg.strip_prefix('[') {
            // 閉じないまま次が始まったら、手前は壊れているので捨てる
            open = Some((okuri.to_string(), Vec::new()));
        } else if seg.starts_with(']') {
            if let Some((okuri, texts)) = open.take()
                && !okuri.is_empty()
                && !texts.is_empty()
            {
                blocks.insert(okuri, texts);
            }
        } else if let Some((_, texts)) = open.as_mut() {
            if let Some(c) = Candidate::parse(seg) {
                texts.push(c.text);
            }
        } else if let Some(c) = Candidate::parse(seg) {
            cands.push(c);
        }
    }
    if cands.is_empty() {
        return None;
    }
    Some((key.to_string(), cands, blocks))
}

/// EUC-JP でも UTF-8 でも読めるように、まず UTF-8 を試して駄目なら EUC-JP とみなす。
fn read_jisyo(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("辞書を読めない: {}", path.display()))?;
    Ok(decode_jisyo(&bytes))
}

/// 辞書のバイト列を文字にする。
///
/// SKK の辞書は実装によって符号化が違う。**BOM があれば信じ、無ければ UTF-8 を
/// 試して、駄目なら EUC-JP** とみなす。
///
/// - EUC-JP … `SKK-JISYO.L` など、古くからの配布物
/// - UTF-8 … 最近の実装 (ttyskk 自身、skkeleton)
/// - UTF-16 … CorvusSKK の利用者辞書 (`userdict.txt`) が LE + BOM で書く
fn decode_jisyo(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return encoding_rs::UTF_16LE.decode(rest).0.into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return encoding_rs::UTF_16BE.decode(rest).0.into_owned();
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::EUC_JP.decode(bytes).0.into_owned(),
    }
}

fn load_into(
    map: &mut HashMap<String, Vec<Candidate>>,
    okuri: &mut HashMap<String, OkuriBlocks>,
    text: &str,
) {
    for line in text.lines() {
        if let Some((key, cands, blocks)) = parse_line(line) {
            let entry = map.entry(key.clone()).or_default();
            for c in cands {
                if !entry.iter().any(|e| e.text == c.text) {
                    entry.push(c);
                }
            }
            if blocks.is_empty() {
                continue;
            }
            let dst = okuri.entry(key).or_default();
            for (o, texts) in blocks {
                let v = dst.entry(o).or_default();
                for t in texts {
                    if !v.contains(&t) {
                        v.push(t);
                    }
                }
            }
        }
    }
}

/// 記録を一つ適用する。保存と読み直しで同じ手順を使う。
fn apply_change(
    change: &Change,
    map: &mut HashMap<String, Vec<Candidate>>,
    okuri: &mut HashMap<String, OkuriBlocks>,
) {
    match change {
        Change::Learn(key, cand) => move_to_front(map.entry(key.clone()).or_default(), cand),
        Change::LearnOkuri(key, o, text) => {
            let v = okuri
                .entry(key.clone())
                .or_default()
                .entry(o.clone())
                .or_default();
            v.retain(|t| t != text);
            v.insert(0, text.clone());
        }
        Change::Purge(key, text) => {
            if let Some(v) = map.get_mut(key) {
                v.retain(|c| c.text != *text);
                if v.is_empty() {
                    map.remove(key);
                }
            }
            // **送り仮名ごとの宛先からも消す。** 片方だけ残すと、消したはずの候補が
            // 送り仮名の一致で先頭に返り咲く。
            if let Some(b) = okuri.get_mut(key) {
                b.retain(|_, v| {
                    v.retain(|t| t != text);
                    !v.is_empty()
                });
                if b.is_empty() {
                    okuri.remove(key);
                }
            }
        }
    }
}

impl Dict {
    /// 共有辞書と利用者辞書を読み込む。共有辞書は見つかったものを全て重ねる。
    pub fn load(
        system_paths: &[PathBuf],
        user_path: PathBuf,
        import: Option<&Path>,
    ) -> Result<Self> {
        let mut system = HashMap::new();
        let mut system_okuri = HashMap::new();
        for p in system_paths {
            if p.exists() {
                load_into(&mut system, &mut system_okuri, &read_jisyo(p)?);
            }
        }
        // 同梱の補助辞書は最後に重ねる。共有辞書に同じ見出し語があればそちらが先。
        load_into(&mut system, &mut system_okuri, BUILTIN);

        let import_path = import.map(|p| p.to_path_buf());
        let mut user = HashMap::new();
        let mut user_okuri = HashMap::new();
        if user_path.exists() {
            load_into(&mut user, &mut user_okuri, &read_jisyo(&user_path)?);
        } else if let Some(src) = &import_path {
            // 初回のみ fcitx5-skk の学習内容を引き継ぐ
            if src.exists() {
                load_into(&mut user, &mut user_okuri, &read_jisyo(src)?);
            }
        }

        let mut system_sorted: Vec<String> = system
            .keys()
            .filter(|k| !is_okuri_ari(k))
            .cloned()
            .collect();
        system_sorted.sort_unstable();

        let user_stamp = stamp(&user_path);
        Ok(Dict {
            system,
            user,
            system_okuri,
            user_okuri,
            snippets: HashMap::new(),
            snippet_paths: Vec::new(),
            snippet_stamps: Vec::new(),
            user_path,
            import_path,
            changes: Vec::new(),
            user_stamp,
            system_sorted,
        })
    }

    /// スニペットのファイルを読む。読めた見出し語の数を返す。
    ///
    /// 何度呼んでもよい (そのつど読み直す)。**壊れたファイルがあっても他は読む** —
    /// 手で書くものなので、書きかけの一つで全部が使えなくなると困る。読めなかった
    /// ファイルは呼んだ側へ返し、知らせるかどうかは任せる。
    pub fn load_snippets(&mut self, paths: &[PathBuf]) -> (usize, Vec<(PathBuf, anyhow::Error)>) {
        self.snippet_paths = paths.to_vec();
        self.snippet_stamps = paths.iter().map(|p| stamp(p)).collect();
        self.snippets.clear();

        let mut failed = Vec::new();
        for path in paths {
            if !path.exists() {
                continue;
            }
            let text = match read_jisyo(path) {
                Ok(t) => t,
                Err(e) => {
                    failed.push((path.clone(), e));
                    continue;
                }
            };
            match crate::snippet::parse(&text) {
                Ok(list) => {
                    for s in list {
                        let entry = self.snippets.entry(s.prefix).or_default();
                        if !entry.iter().any(|c| c.text == s.body) {
                            entry.push(Candidate {
                                text: s.body,
                                annotation: s.description,
                            });
                        }
                    }
                }
                Err(e) => failed.push((path.clone(), e)),
            }
        }
        (self.snippets.len(), failed)
    }

    /// スニペットが編集器で書き換えられていたら読み直す。読んだら true。
    pub fn reload_snippets(&mut self) -> (bool, Vec<(PathBuf, anyhow::Error)>) {
        let now: Vec<_> = self.snippet_paths.iter().map(|p| stamp(p)).collect();
        if now == self.snippet_stamps {
            return (false, Vec::new());
        }
        let paths = std::mem::take(&mut self.snippet_paths);
        let (_, failed) = self.load_snippets(&paths);
        (true, failed)
    }

    /// スニペットの見出し語の数。
    pub fn snippet_len(&self) -> usize {
        self.snippets.len()
    }

    /// その候補が定型文から来たものか。
    ///
    /// 埋める場所 (`$1` など) を探すのは定型文だけに限るために要る。TextMate の
    /// 決まりでは `$100` も埋め場所なので、共有辞書に `$` を含む候補があっても
    /// 巻き込まないようにする。
    pub fn is_snippet(&self, key: &str, text: &str) -> bool {
        self.snippets
            .get(key)
            .is_some_and(|v| v.iter().any(|c| c.text == text))
    }

    /// 共有辞書の見出し語数。
    /// 共有辞書の見出し語と候補を見て回る (並びは決まっていない)。
    ///
    /// **送りありの見出し語は含まない。** 読みが途中で切れていて前方一致に使えないので、
    /// 索引に入れても引けない。ただし**英字だけの見出し** (`a /エー/`、`note /ノート/`)
    /// は送りありに見えるだけの別物なので残す — 打った字面で引く道がここにある。
    ///
    /// migemo の索引を作るために使う。
    pub fn system_entries(&self) -> impl Iterator<Item = (&str, &[Candidate])> {
        self.system
            .iter()
            .filter(|(k, _)| !is_okuri_ari(k) || k.is_ascii())
            .map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    pub fn system_len(&self) -> usize {
        self.system.len()
    }

    /// 別の SKK 辞書を利用者辞書に取り込む。足した候補の数を返す。
    ///
    /// 他の実装 (skkeleton の `~/.skkeleton`、fcitx5-skk の `user.dict` など) で
    /// 溜めた学習を合流させるためのもの。**既にある候補は動かさない** — 学習の
    /// 順序は「最近使った順」なので、取り込んだものを先頭に置くと、いま使っている
    /// 語より古い語が前に出てしまう。相手にしかない候補だけを後ろへ足す。
    ///
    /// 保存と同じく、ディスクの現状を読み直してから重ねる。取り込みの最中に他の
    /// プロセスが覚えたことも失わない。
    pub fn import_user(&mut self, path: &Path) -> Result<usize> {
        let mut incoming = HashMap::new();
        let mut incoming_okuri = HashMap::new();
        load_into(&mut incoming, &mut incoming_okuri, &read_jisyo(path)?);

        // ディスクの現状 + この起動で覚えたことを土台にする
        let (mut merged, mut okuri) = self.merged_with_disk()?;
        let mut added = 0;
        for (key, cands) in incoming {
            let entry = merged.entry(key).or_default();
            for c in cands {
                if !entry.iter().any(|e| e.text == c.text) {
                    entry.push(c);
                    added += 1;
                }
            }
        }
        // **送り仮名ごとの宛先も引き継ぐ。** ddskk や CorvusSKK で溜めた仕分けは、
        // 見出し語ごとの候補と同じだけ学習の成果にあたる。候補と同じく後ろへ足す。
        for (key, blocks) in incoming_okuri {
            let dst = okuri.entry(key).or_default();
            for (o, texts) in blocks {
                let v = dst.entry(o).or_default();
                for t in texts {
                    if !v.contains(&t) {
                        v.push(t);
                    }
                }
            }
        }
        self.write_user(merged, okuri)?;
        Ok(added)
    }

    /// 利用者辞書が別のプロセスに書き換えられていたら読み直す。読んだら true。
    ///
    /// GUI の入力メソッドや、別のペインの ttyskk が覚えたことを取り込むためのもの。
    /// **この起動で覚えたことは保つ** — ディスクの内容を土台に、自分の記録
    /// (`changes`) を上から重ねる。保存のときと同じ順序なので、どちらが先に書いても
    /// 結果は変わらない。
    ///
    /// 自分で保存した直後は目印を更新してあるので、ここで読み直しは起きない。
    pub fn reload_user(&mut self) -> Result<bool> {
        let now = stamp(&self.user_path);
        if now == self.user_stamp {
            return Ok(false);
        }
        self.user_stamp = now;

        let mut user = HashMap::new();
        let mut user_okuri = HashMap::new();
        if self.user_path.exists() {
            load_into(&mut user, &mut user_okuri, &read_jisyo(&self.user_path)?);
        }
        for change in &self.changes {
            apply_change(change, &mut user, &mut user_okuri);
        }
        self.user = user;
        self.user_okuri = user_okuri;
        Ok(true)
    }

    /// 見出し語を引く。利用者辞書・スニペット・共有辞書の順に重ねる。
    ///
    /// スニペットが共有辞書より前に来るのは、手で書いたものだから — 「でんわ」に
    /// 自分の番号を書いたなら、`SKK-JISYO.L` の「電話」より先に出したい。
    pub fn lookup(&self, key: &str) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        if let Some(v) = self.user.get(key) {
            out.extend(v.iter().cloned());
        }
        for src in [&self.snippets, &self.system] {
            if let Some(v) = src.get(key) {
                for c in v {
                    if !out.iter().any(|e| e.text == c.text) {
                        out.push(c.clone());
                    }
                }
            }
        }
        out
    }

    /// その送り仮名に登録されている候補の本文。利用者辞書が先、共有辞書が後ろ。
    ///
    /// 並べ替えるかどうかは呼ぶ側が決める (設定を持っているのはそちら)。ここは
    /// 「何が登録されているか」だけを答える。
    pub fn okuri_candidates(&self, key: &str, okuri: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for src in [&self.user_okuri, &self.system_okuri] {
            let Some(v) = src.get(key).and_then(|b| b.get(okuri)) else {
                continue;
            };
            for t in v {
                if !out.contains(t) {
                    out.push(t.clone());
                }
            }
        }
        out
    }

    /// 前方一致する見出し語を集める (TAB 補完)。
    ///
    /// 利用者辞書のものを先に置く。そこにあるのは実際に使った語なので、
    /// 共有辞書の 17 万語から拾ったものより当たりやすい。同じ長さなら辞書順。
    /// 送りありの見出し語 (`うごk`) は補完しても打ち直せないので外す。
    pub fn complete(&self, prefix: &str, limit: usize) -> Vec<String> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let order =
            |a: &String, b: &String| a.chars().count().cmp(&b.chars().count()).then(a.cmp(b));

        // 利用者辞書とスニペットは数千語なので素直に舐める (増減するため索引を持たない)
        let mut out: Vec<String> = self
            .user
            .keys()
            .chain(self.snippets.keys())
            .filter(|k| k.len() > prefix.len() && k.starts_with(prefix) && !is_okuri_ari(k))
            .cloned()
            .collect();
        out.sort_by(&order);
        out.dedup();
        out.truncate(limit);

        let mut rest: Vec<String> = self
            .system_prefix_range(prefix)
            .iter()
            .filter(|k| k.len() > prefix.len())
            .cloned()
            .collect();
        rest.sort_by(&order);
        for k in rest {
            if out.len() >= limit {
                break;
            }
            if !out.contains(&k) {
                out.push(k);
            }
        }
        out
    }

    /// 共有辞書の索引から、前方一致する範囲を切り出す。
    fn system_prefix_range(&self, prefix: &str) -> &[String] {
        let v = &self.system_sorted;
        let lo = v.partition_point(|k| k.as_str() < prefix);
        let hi = v.partition_point(|k| k.as_str() < prefix || k.starts_with(prefix));
        &v[lo..hi]
    }

    /// 確定した候補を利用者辞書の先頭に移す (学習)。
    ///
    /// **スニペットの候補は覚えない。** あれは編集器で書いて編集器で直すもので、
    /// 並びもファイルの順がそのまま出る。写してしまうと、住所を書き換えたときに
    /// 古い住所が利用者辞書に残って先頭に出続ける。
    pub fn learn(&mut self, key: &str, cand: &Candidate) {
        if self
            .snippets
            .get(key)
            .is_some_and(|v| v.iter().any(|c| c.text == cand.text))
        {
            return;
        }
        move_to_front(self.user.entry(key.to_string()).or_default(), cand);
        self.changes
            .push(Change::Learn(key.to_string(), cand.clone()));
    }

    /// 送り仮名ごとの宛先へ覚える。
    ///
    /// [`Dict::learn`] と対で呼ぶ。あちらが「この見出し語で使った」を、こちらが
    /// 「この送り仮名で使った」を記録する。両方あって初めて、`おおk` の中で
    /// 「大きい」と「多く」が分かれる。
    pub fn learn_okuri(&mut self, key: &str, okuri: &str, text: &str) {
        if okuri.is_empty() || text.is_empty() {
            return;
        }
        // 定型文は編集器で書いて編集器で直すもの。学習では動かさない ([`Dict::learn`])
        if self.is_snippet(key, text) {
            return;
        }
        let change = Change::LearnOkuri(key.to_string(), okuri.to_string(), text.to_string());
        apply_change(&change, &mut self.user, &mut self.user_okuri);
        self.changes.push(change);
    }

    /// 候補を利用者辞書から取り除く。
    ///
    /// 手元に無くても記録は残す。別のペインが覚えた分がディスクにある場合、
    /// 保存時にそちらから消す必要があるため。共有辞書には手を触れないので、
    /// そちら由来の候補は次も出る (学習による先頭への繰り上がりだけが消える)。
    pub fn purge(&mut self, key: &str, text: &str) {
        let change = Change::Purge(key.to_string(), text.to_string());
        apply_change(&change, &mut self.user, &mut self.user_okuri);
        self.changes.push(change);
    }

    /// 利用者辞書を書き出す。覚えたことがなければ何もしない。
    pub fn save(&mut self) -> Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }
        let (merged, okuri) = self.merged_with_disk()?;
        self.write_user(merged, okuri)
    }

    /// ディスクの現状に、この起動で覚えたことを重ねたもの。
    ///
    /// 丸ごと書き出すと、複数の ttyskk (端末・GUI・別のペイン) が互いの学習を消し
    /// 合ってしまう。**ディスクを土台にして自分の記録だけを重ねる**ことで、他が
    /// 覚えたことを残したまま書ける。
    #[allow(clippy::type_complexity)]
    fn merged_with_disk(
        &self,
    ) -> Result<(
        HashMap<String, Vec<Candidate>>,
        HashMap<String, OkuriBlocks>,
    )> {
        let mut merged = HashMap::new();
        let mut okuri = HashMap::new();
        if self.user_path.exists() {
            load_into(&mut merged, &mut okuri, &read_jisyo(&self.user_path)?);
        } else if let Some(src) = &self.import_path
            && src.exists()
        {
            load_into(&mut merged, &mut okuri, &read_jisyo(src)?);
        }
        for change in &self.changes {
            apply_change(change, &mut merged, &mut okuri);
        }
        Ok((merged, okuri))
    }

    /// 利用者辞書を書き出し、手元の状態を書いた内容に合わせる。
    fn write_user(
        &mut self,
        merged: HashMap<String, Vec<Candidate>>,
        okuri: HashMap<String, OkuriBlocks>,
    ) -> Result<()> {
        if let Some(dir) = self.user_path.parent() {
            fs::create_dir_all(dir)?;
        }
        // 書き込み中に落ちても元が壊れないよう、一時ファイル経由で置き換える
        let tmp = self.user_path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            writeln!(f, ";; -*- mode: fundamental; coding: utf-8 -*-")?;
            writeln!(f, ";; ttyskk 利用者辞書")?;

            let mut keys: Vec<&String> = merged.keys().collect();
            keys.sort();

            let empty = OkuriBlocks::new();
            writeln!(f, ";; okuri-ari entries.")?;
            for k in keys.iter().filter(|k| is_okuri_ari(k)) {
                write_entry(&mut f, k, &merged[*k], okuri.get(*k).unwrap_or(&empty))?;
            }
            writeln!(f, ";; okuri-nasi entries.")?;
            for k in keys.iter().filter(|k| !is_okuri_ari(k)) {
                write_entry(&mut f, k, &merged[*k], &empty)?;
            }
        }
        fs::rename(&tmp, &self.user_path)?;
        self.changes.clear();
        self.user = merged;
        self.user_okuri = okuri;
        self.user_stamp = stamp(&self.user_path);
        Ok(())
    }
}

/// ファイルの「いまの状態」。存在しない場合も含めて表す。
///
/// 更新時刻と大きさの組で見る。編集器が別名で書いて置き換える場合も、消してから
/// 作り直す場合も同じように拾える (設定ファイルの見張りと同じ考え方)。
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn move_to_front(entry: &mut Vec<Candidate>, cand: &Candidate) {
    entry.retain(|c| c.text != cand.text);
    entry.insert(0, cand.clone());
}

fn is_okuri_ari(key: &str) -> bool {
    key.chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphabetic())
}

fn write_entry(
    f: &mut fs::File,
    key: &str,
    cands: &[Candidate],
    okuri: &OkuriBlocks,
) -> std::io::Result<()> {
    if cands.is_empty() {
        return Ok(());
    }
    write!(f, "{} /", key)?;
    for c in cands {
        write!(f, "{}/", c.serialize())?;
    }
    // 送り仮名ごとの宛先は候補列の後ろ。ddskk・CorvusSKK と同じ並べ方なので、
    // 同じファイルを分け合っても互いに読める。
    for (o, texts) in okuri {
        if texts.is_empty() {
            continue;
        }
        write!(f, "[{o}/")?;
        for t in texts {
            write!(f, "{t}/")?;
        }
        write!(f, "]/")?;
    }
    writeln!(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辞書の本文を読み、候補の表と送り仮名ごとの宛先を返す。
    fn loaded(text: &str) -> (HashMap<String, Vec<Candidate>>, HashMap<String, OkuriBlocks>) {
        let (mut map, mut okuri) = (HashMap::new(), HashMap::new());
        load_into(&mut map, &mut okuri, text);
        (map, okuri)
    }

    #[test]
    fn builtin_covers_the_circled_numbers() {
        let (m, _) = loaded(BUILTIN);
        assert_eq!(m.len(), 100, "まる1〜50 と c1〜50 の二通り");
        for (key, want) in [
            ("まる1", "①"),
            ("まる20", "⑳"),
            ("まる21", "㉑"),
            ("まる35", "㉟"),
            ("まる36", "㊱"),
            ("まる50", "㊿"),
            ("c1", "①"),
            ("c20", "⑳"),
            ("c21", "㉑"),
            ("c50", "㊿"),
        ] {
            assert_eq!(m[key][0].text, want, "{key}");
        }
    }

    #[test]
    fn blank_candidates_are_dropped() {
        // 登録に失敗した跡。共有辞書の正しい候補を覆い隠すので拾わない。
        assert!(parse_line("まる1 / /").is_none());
        assert!(parse_line("あ ///").is_none());
        // 空候補が混じっていても、まともな候補は残る
        let (_, c, _) = parse_line("あ /亜// /唖/").unwrap();
        assert_eq!(
            c.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["亜", "唖"]
        );
        // 全角空白は正当な候補
        let (_, c, _) = parse_line("すぺーす /　/").unwrap();
        assert_eq!(c[0].text, "　");
    }

    #[test]
    fn completes_by_prefix() {
        let (sys, system_okuri) = loaded(
            "かんじ /漢字/\nかんじゃ /患者/\nかんきょう /環境/\nかい /回/\nかんがr /考/\nかん /缶/\n",
        );
        let (user, user_okuri) = loaded("かんきょう /環境/\nかんぱい /乾杯/\n");
        let mut system_sorted: Vec<String> =
            sys.keys().filter(|k| !is_okuri_ari(k)).cloned().collect();
        system_sorted.sort_unstable();
        let d = Dict {
            system: sys,
            user,
            system_okuri,
            user_okuri,
            snippets: HashMap::new(),
            snippet_paths: Vec::new(),
            snippet_stamps: Vec::new(),
            user_path: PathBuf::from("/dev/null"),
            import_path: None,
            changes: Vec::new(),
            user_stamp: None,
            system_sorted,
        };
        // 利用者辞書のものが先、そのあと共有辞書。同じ長さなら辞書順。
        assert_eq!(
            d.complete("かん", 10),
            ["かんぱい", "かんきょう", "かんじ", "かんじゃ"]
        );
        // 送りありの見出し語 (かんがr) は出さない。完全一致 (かん) も出さない。
        assert!(!d.complete("かん", 10).iter().any(|k| k.ends_with('r')));
        assert!(!d.complete("かん", 10).contains(&"かん".to_string()));
        assert_eq!(d.complete("かん", 2).len(), 2, "上限が効く");
        assert!(d.complete("", 10).is_empty());
        assert!(d.complete("ぬ", 10).is_empty());
    }

    #[test]
    fn purge_survives_the_merge_with_disk() {
        let dir = std::env::temp_dir().join(format!("ttyskk-purge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.dict");
        std::fs::write(
            &path,
            ";; okuri-ari entries.\n;; okuri-nasi entries.\nかんじ /幹事/漢字/\n",
        )
        .unwrap();

        let mut d = Dict::load(&[], path.clone(), None).unwrap();
        d.purge("かんじ", "幹事");
        assert_eq!(d.lookup("かんじ").len(), 1);
        d.save().unwrap();

        // 読み直しても消えたまま
        let d2 = Dict::load(&[], path.clone(), None).unwrap();
        assert_eq!(
            d2.lookup("かんじ")
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>(),
            ["漢字"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 送り仮名ごとの宛先を、覚えて・書き出して・読み直せる。
    ///
    /// **他の実装が書いたものを消さないことが要**。README にある「利用者辞書を git で
    /// 分け合う」使い方で ddskk と混ぜても壊さない。
    #[test]
    fn okuri_blocks_survive_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("ttyskk-okuri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.dict");
        // 手を触れていない、別の実装が書いた宛先 (うごk の [き/起/])
        std::fs::write(
            &path,
            ";; okuri-ari entries.\nうごk /動/起/[き/起/]/\n;; okuri-nasi entries.\n",
        )
        .unwrap();

        let mut d = Dict::load(&[], path.clone(), None).unwrap();
        assert_eq!(d.okuri_candidates("うごk", "き"), ["起"], "読める");
        d.learn(
            "おおk",
            &Candidate {
                text: "多".into(),
                annotation: None,
            },
        );
        d.learn_okuri("おおk", "く", "多");
        d.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("おおk /多/[く/多/]/"), "書き出す形: {text}");
        assert!(
            text.contains("うごk /動/起/[き/起/]/"),
            "触っていない宛先を消さない: {text}"
        );

        let d2 = Dict::load(&[], path.clone(), None).unwrap();
        assert_eq!(d2.okuri_candidates("おおk", "く"), ["多"]);
        assert_eq!(d2.okuri_candidates("うごk", "き"), ["起"]);
        assert!(d2.okuri_candidates("おおk", "き").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 候補を削除したら、送り仮名ごとの宛先からも消える。
    ///
    /// 片方だけ残すと、消したはずの候補が送り仮名の一致で先頭に返り咲く。
    #[test]
    fn purge_also_clears_the_okuri_blocks() {
        let dir = std::env::temp_dir().join(format!("ttyskk-okuri-purge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.dict");
        std::fs::write(
            &path,
            ";; okuri-ari entries.\nおおk /多/大/[く/多/]/[き/大/]/\n;; okuri-nasi entries.\n",
        )
        .unwrap();

        let mut d = Dict::load(&[], path.clone(), None).unwrap();
        d.purge("おおk", "多");
        assert!(d.okuri_candidates("おおk", "く").is_empty(), "宛先も消える");
        assert_eq!(d.okuri_candidates("おおk", "き"), ["大"], "他は残る");

        d.save().unwrap();
        let d2 = Dict::load(&[], path.clone(), None).unwrap();
        assert!(d2.okuri_candidates("おおk", "く").is_empty());
        assert_eq!(d2.okuri_candidates("おおk", "き"), ["大"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CorvusSKK の利用者辞書 (UTF-16LE + BOM、送り仮名ブロック付き) を読める。
    #[test]
    fn reads_corvusskk_user_dictionary() {
        // UTF-16LE + BOM で書かれた、送り仮名ブロックを含む辞書
        let text = ";; okuri-ari entries.\n\
                    おくr /送/[り/送/]/\n\
                    ;; okuri-nasi entries.\n\
                    かんじ /漢字/幹事/\n";
        let mut bytes = vec![0xff, 0xfe];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let decoded = decode_jisyo(&bytes);
        assert_eq!(decoded, text, "UTF-16LE を読める");

        let (map, okuri) = loaded(&decoded);
        // 候補列に `[り` や `]` が紛れ込まない
        let cands: Vec<&str> = map["おくr"].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(cands, ["送"], "[り/送/] は候補に混ぜない");
        assert_eq!(map["かんじ"].len(), 2);
        // 送り仮名ごとの宛先は宛先として取る (捨てない)
        assert_eq!(okuri["おくr"]["り"], ["送"]);
        assert!(!okuri.contains_key("かんじ"), "送りなしには付かない");
    }

    /// 符号化は BOM を信じ、無ければ UTF-8 → EUC-JP の順に試す。
    #[test]
    fn decodes_the_usual_encodings() {
        assert_eq!(decode_jisyo("かんじ /漢字/".as_bytes()), "かんじ /漢字/");
        // BOM 付きの UTF-8
        let mut utf8_bom = vec![0xef, 0xbb, 0xbf];
        utf8_bom.extend_from_slice("あ /亜/".as_bytes());
        assert_eq!(decode_jisyo(&utf8_bom), "あ /亜/");
        // EUC-JP
        let (euc, _, _) = encoding_rs::EUC_JP.encode("あ /亜/");
        assert_eq!(decode_jisyo(&euc), "あ /亜/");
    }

    /// 他の実装で溜めた学習を合流させる。
    ///
    /// **既にある候補は動かさない** — 学習の順序は「最近使った順」なので、取り込んだ
    /// ものを先頭に置くと、いま使っている語より古い語が前に出てしまう。
    #[test]
    fn imports_another_implementations_learning() {
        let dir = std::env::temp_dir().join(format!("ttyskk-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.dict");
        let other = dir.join("skkeleton");
        std::fs::write(&user, ";; okuri-nasi entries.\nかんじ /漢字/幹事/\n").unwrap();
        // 向こうにしかない語と、こちらと順序が違う語
        std::fs::write(
            &other,
            ";; okuri-ari entries.\nうごk /動/\n;; okuri-nasi entries.\nかんじ /監事/幹事/\nとうきょう /東京/\n",
        )
        .unwrap();

        let mut d = Dict::load(&[], user.clone(), None).unwrap();
        let added = d.import_user(&other).unwrap();
        assert_eq!(added, 3, "監事・東京・動 の 3 つ");

        // こちらの順序は変わらない。向こうにしかないものが後ろに付く
        let k: Vec<String> = d.lookup("かんじ").into_iter().map(|c| c.text).collect();
        assert_eq!(k, ["漢字", "幹事", "監事"]);
        assert_eq!(d.lookup("とうきょう")[0].text, "東京");
        assert_eq!(d.lookup("うごk")[0].text, "動");

        // ディスクにも書けている
        let fresh = Dict::load(&[], user, None).unwrap();
        assert_eq!(fresh.lookup("とうきょう")[0].text, "東京");
        assert_eq!(fresh.lookup("うごk")[0].text, "動");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 別のプロセスが覚えたことを、起動し直さずに取り込む。
    ///
    /// GUI の入力メソッドと端末の ttyskk が同じ利用者辞書を使うので、**動いている
    /// 最中に外から書き換わる**。自分がこの起動で覚えたことは失わない。
    #[test]
    fn reloads_what_another_process_learned() {
        let dir = std::env::temp_dir().join(format!("ttyskk-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("user.dict");
        std::fs::write(&path, ";; okuri-nasi entries.\nかんじ /漢字/幹事/\n").unwrap();

        let mut d = Dict::load(&[], path.clone(), None).unwrap();
        // こちらは「にほん」を覚えた (まだ書き出していない)
        d.learn(
            "にほん",
            &Candidate {
                text: "日本".into(),
                annotation: None,
            },
        );

        // その間に別のプロセスが「かんじ」の並びを変え、「とうきょう」を足した
        std::fs::write(
            &path,
            ";; okuri-nasi entries.\nかんじ /幹事/漢字/\nとうきょう /東京/\n",
        )
        .unwrap();

        assert!(d.reload_user().unwrap(), "書き換わっていたので読み直す");
        // 向こうの学習が入り
        assert_eq!(d.lookup("かんじ")[0].text, "幹事");
        assert_eq!(d.lookup("とうきょう")[0].text, "東京");
        // こちらの学習も残る
        assert_eq!(d.lookup("にほん")[0].text, "日本");

        // 変わっていなければ読み直さない
        assert!(!d.reload_user().unwrap());

        // 自分で保存した直後も読み直しは起きない (目印を更新するため)
        d.save().unwrap();
        assert!(!d.reload_user().unwrap());
        // 保存した内容には両方入っている
        let fresh = Dict::load(&[], path.clone(), None).unwrap();
        assert_eq!(fresh.lookup("にほん")[0].text, "日本");
        assert_eq!(fresh.lookup("とうきょう")[0].text, "東京");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// スニペットは共有辞書より先に出て、確定しても利用者辞書へ写らない。
    ///
    /// 写してしまうと、住所を書き換えたときに古い住所が先頭に残り続ける。
    #[test]
    fn snippets_come_before_the_shared_dictionary_and_are_never_learned() {
        let dir = std::env::temp_dir().join(format!("ttyskk-snip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = dir.join("sys.dict");
        std::fs::write(&sys, "でんわ /電話/\n").unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(
            &snip,
            r#"{"会社の電話": {"prefix": "でんわ", "body": "03-1111-2222", "description": "会社"}}"#,
        )
        .unwrap();

        let mut d = Dict::load(std::slice::from_ref(&sys), dir.join("user.dict"), None).unwrap();
        let (n, failed) = d.load_snippets(std::slice::from_ref(&snip));
        assert_eq!((n, failed.len()), (1, 0));

        // スニペットが先、共有辞書は後ろ
        let got: Vec<String> = d.lookup("でんわ").into_iter().map(|c| c.text).collect();
        assert_eq!(got, ["03-1111-2222", "電話"]);
        assert_eq!(d.lookup("でんわ")[0].annotation.as_deref(), Some("会社"));

        // 確定しても利用者辞書へ写らない (保存するものが無い)
        let cand = d.lookup("でんわ")[0].clone();
        d.learn("でんわ", &cand);
        d.save().unwrap();
        assert!(!dir.join("user.dict").exists(), "書き出すものは無いはず");

        // 共有辞書の候補はこれまでどおり覚える
        let denwa = Candidate {
            text: "電話".into(),
            annotation: None,
        };
        d.learn("でんわ", &denwa);
        d.save().unwrap();
        let fresh = Dict::load(&[], dir.join("user.dict"), None).unwrap();
        assert_eq!(fresh.lookup("でんわ")[0].text, "電話");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 編集器で保存されたら読み直す。壊れていても前の内容を捨てない。
    #[test]
    fn reloads_snippets_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("ttyskk-snipre-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("s.code-snippets");
        std::fs::write(
            &snip,
            r#"{"住所": {"prefix": "じゅうしょ", "body": "港区"}}"#,
        )
        .unwrap();

        let mut d = Dict::load(&[], dir.join("user.dict"), None).unwrap();
        d.load_snippets(std::slice::from_ref(&snip));
        assert_eq!(d.lookup("じゅうしょ")[0].text, "港区");
        // 変わっていなければ読み直さない
        assert!(!d.reload_snippets().0);

        // 引っ越したので書き換える。時刻の粒度に負けないよう長さも変える。
        std::fs::write(
            &snip,
            r#"{"住所": {"prefix": "じゅうしょ", "body": "千代田区一番町"}}"#,
        )
        .unwrap();
        let (reloaded, failed) = d.reload_snippets();
        assert!(reloaded && failed.is_empty());
        // 古い住所は残らない
        let got: Vec<String> = d.lookup("じゅうしょ").into_iter().map(|c| c.text).collect();
        assert_eq!(got, ["千代田区一番町"]);

        // 補完でも見つかる
        assert!(d.complete("じゅう", 10).contains(&"じゅうしょ".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 壊れたファイルがあっても、他のファイルは読める。
    #[test]
    fn a_broken_file_does_not_stop_the_others() {
        let dir = std::env::temp_dir().join(format!("ttyskk-snipbad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.code-snippets");
        let good = dir.join("good.code-snippets");
        std::fs::write(&bad, r#"{"壊れ": {"prefix": "あ" "body": "亜"}}"#).unwrap();
        std::fs::write(&good, r#"{"良": {"prefix": "い", "body": "居"}}"#).unwrap();

        let mut d = Dict::load(&[], dir.join("user.dict"), None).unwrap();
        let (n, failed) = d.load_snippets(&[bad.clone(), good]);
        assert_eq!(n, 1, "良い方は読める");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, bad);
        assert_eq!(d.lookup("い")[0].text, "居");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_entries() {
        let (k, c, o) = parse_line("かんじ /漢字/幹事;幹事さん/").unwrap();
        assert_eq!(k, "かんじ");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "漢字");
        assert_eq!(c[1].text, "幹事");
        assert_eq!(c[1].annotation.as_deref(), Some("幹事さん"));
        assert!(o.is_empty());
    }

    /// 送り仮名ブロックの読み取り。`/` で素朴に分けると `[き` と `]` に散る。
    #[test]
    fn parses_okuri_blocks() {
        let (k, c, o) = parse_line("おおk /大/多/[き/大/]/[く/多/夛/]/").unwrap();
        assert_eq!(k, "おおk");
        assert_eq!(
            c.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["大", "多"],
            "括弧の中身は候補に混ぜない"
        );
        assert_eq!(o["き"], ["大"]);
        assert_eq!(o["く"], ["多", "夛"]);

        // 注釈の角括弧 (`[文語]`) はブロックではない。`/` の直後だけが始まりの印。
        let (_, c, o) = parse_line("ゆるb /弛;[文語]/緩;[文語]/").unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].annotation.as_deref(), Some("[文語]"));
        assert!(o.is_empty(), "注釈をブロックと取り違えない");
    }

    #[test]
    fn skips_comments() {
        assert!(parse_line(";; okuri-ari entries.").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn detects_okuri_ari_key() {
        assert!(is_okuri_ari("うごk"));
        assert!(!is_okuri_ari("かんじ"));
    }

    /// 二つの ttyskk が同時に走っても、互いの学習を消さないこと。
    #[test]
    fn save_merges_with_what_is_already_on_disk() {
        let dir = std::env::temp_dir().join(format!("ttyskk-dict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("merge.dict");
        let _ = std::fs::remove_file(&user);
        let sys = dir.join("merge-sys.dict");
        std::fs::write(&sys, "かんじ /漢字/幹事/\nあい /愛/相/\n").unwrap();

        let mut a = Dict::load(std::slice::from_ref(&sys), user.clone(), None).unwrap();
        let mut b = Dict::load(std::slice::from_ref(&sys), user.clone(), None).unwrap();

        // 別々のペインでそれぞれ違う語を覚える
        a.learn(
            "かんじ",
            &Candidate {
                text: "幹事".into(),
                annotation: None,
            },
        );
        b.learn(
            "あい",
            &Candidate {
                text: "相".into(),
                annotation: None,
            },
        );
        a.save().unwrap();
        b.save().unwrap();

        // 後から保存した b が a の学習を消していないこと
        let merged = Dict::load(std::slice::from_ref(&sys), user, None).unwrap();
        assert_eq!(merged.lookup("かんじ")[0].text, "幹事");
        assert_eq!(merged.lookup("あい")[0].text, "相");
    }
}
