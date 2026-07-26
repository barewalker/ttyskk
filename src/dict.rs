//! SKK 辞書の読み込みと引き当て。
//!
//! 見出し語は送りなしが「かな」、送りありが「かな + 送り仮名のローマ字頭文字」
//! (例: 「動く」なら `うごk`)。この二つは同じ表に入れても衝突しないため、
//! ひとつの HashMap で扱う。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

/// 同梱する補助辞書。どの標準辞書にも入っていないが、無いと困るものを持つ。
/// いまは丸数字 (①〜㊿) だけ。バイナリに埋め込むので、置き場所の設定が要らない。
const BUILTIN: &str = include_str!("../dict/SKK-JISYO.ttyskk");

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

/// 辞書一式。利用者辞書を先に引き、次に共有辞書を引く。
pub struct Dict {
    system: HashMap<String, Vec<Candidate>>,
    user: HashMap<String, Vec<Candidate>>,
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
    /// 候補を取り除く
    Purge(String, String),
}

/// 一行を「見出し語」と「候補列」に分ける。
fn parse_line(line: &str) -> Option<(String, Vec<Candidate>)> {
    if line.is_empty() || line.starts_with(';') {
        return None;
    }
    let (key, rest) = line.split_once(' ')?;
    let body = rest.trim();
    let body = body.strip_prefix('/').unwrap_or(body);
    let body = body.strip_suffix('/').unwrap_or(body);
    let cands: Vec<Candidate> = body.split('/').filter_map(Candidate::parse).collect();
    if cands.is_empty() {
        return None;
    }
    Some((key.to_string(), cands))
}

/// EUC-JP でも UTF-8 でも読めるように、まず UTF-8 を試して駄目なら EUC-JP とみなす。
fn read_jisyo(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("辞書を読めない: {}", path.display()))?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => {
            let (s, _, _) = encoding_rs::EUC_JP.decode(e.as_bytes());
            Ok(s.into_owned())
        }
    }
}

fn load_into(map: &mut HashMap<String, Vec<Candidate>>, text: &str) {
    for line in text.lines() {
        if let Some((key, cands)) = parse_line(line) {
            let entry = map.entry(key).or_default();
            for c in cands {
                if !entry.iter().any(|e| e.text == c.text) {
                    entry.push(c);
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
        for p in system_paths {
            if p.exists() {
                load_into(&mut system, &read_jisyo(p)?);
            }
        }
        // 同梱の補助辞書は最後に重ねる。共有辞書に同じ見出し語があればそちらが先。
        load_into(&mut system, BUILTIN);

        let import_path = import.map(|p| p.to_path_buf());
        let mut user = HashMap::new();
        if user_path.exists() {
            load_into(&mut user, &read_jisyo(&user_path)?);
        } else if let Some(src) = &import_path {
            // 初回のみ fcitx5-skk の学習内容を引き継ぐ
            if src.exists() {
                load_into(&mut user, &read_jisyo(src)?);
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
            user_path,
            import_path,
            changes: Vec::new(),
            user_stamp,
            system_sorted,
        })
    }

    /// 共有辞書の見出し語数。
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
        load_into(&mut incoming, &read_jisyo(path)?);

        // ディスクの現状 + この起動で覚えたことを土台にする
        let mut merged = self.merged_with_disk()?;
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
        self.write_user(merged)?;
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
        if self.user_path.exists() {
            load_into(&mut user, &read_jisyo(&self.user_path)?);
        }
        for change in &self.changes {
            match change {
                Change::Learn(key, cand) => {
                    move_to_front(user.entry(key.clone()).or_default(), cand)
                }
                Change::Purge(key, text) => {
                    if let Some(v) = user.get_mut(key) {
                        v.retain(|c| c.text != *text);
                        if v.is_empty() {
                            user.remove(key);
                        }
                    }
                }
            }
        }
        self.user = user;
        Ok(true)
    }

    /// 見出し語を引く。利用者辞書の順序を優先し、共有辞書の候補を後ろに足す。
    pub fn lookup(&self, key: &str) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        if let Some(v) = self.user.get(key) {
            out.extend(v.iter().cloned());
        }
        if let Some(v) = self.system.get(key) {
            for c in v {
                if !out.iter().any(|e| e.text == c.text) {
                    out.push(c.clone());
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

        // 利用者辞書は数千語なので素直に舐める (学習で増減するため索引を持たない)
        let mut out: Vec<String> = self
            .user
            .keys()
            .filter(|k| k.len() > prefix.len() && k.starts_with(prefix) && !is_okuri_ari(k))
            .cloned()
            .collect();
        out.sort_by(&order);
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
    pub fn learn(&mut self, key: &str, cand: &Candidate) {
        move_to_front(self.user.entry(key.to_string()).or_default(), cand);
        self.changes
            .push(Change::Learn(key.to_string(), cand.clone()));
    }

    /// 候補を利用者辞書から取り除く。
    ///
    /// 手元に無くても記録は残す。別のペインが覚えた分がディスクにある場合、
    /// 保存時にそちらから消す必要があるため。共有辞書には手を触れないので、
    /// そちら由来の候補は次も出る (学習による先頭への繰り上がりだけが消える)。
    pub fn purge(&mut self, key: &str, text: &str) {
        if let Some(v) = self.user.get_mut(key) {
            v.retain(|c| c.text != text);
            if v.is_empty() {
                self.user.remove(key);
            }
        }
        self.changes
            .push(Change::Purge(key.to_string(), text.to_string()));
    }

    /// 利用者辞書を書き出す。覚えたことがなければ何もしない。
    pub fn save(&mut self) -> Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }
        let merged = self.merged_with_disk()?;
        self.write_user(merged)
    }

    /// ディスクの現状に、この起動で覚えたことを重ねたもの。
    ///
    /// 丸ごと書き出すと、複数の ttyskk (端末・GUI・別のペイン) が互いの学習を消し
    /// 合ってしまう。**ディスクを土台にして自分の記録だけを重ねる**ことで、他が
    /// 覚えたことを残したまま書ける。
    fn merged_with_disk(&self) -> Result<HashMap<String, Vec<Candidate>>> {
        let mut merged = HashMap::new();
        if self.user_path.exists() {
            load_into(&mut merged, &read_jisyo(&self.user_path)?);
        } else if let Some(src) = &self.import_path
            && src.exists()
        {
            load_into(&mut merged, &read_jisyo(src)?);
        }
        for change in &self.changes {
            match change {
                Change::Learn(key, cand) => {
                    move_to_front(merged.entry(key.clone()).or_default(), cand)
                }
                Change::Purge(key, text) => {
                    if let Some(v) = merged.get_mut(key) {
                        v.retain(|c| c.text != *text);
                        if v.is_empty() {
                            merged.remove(key);
                        }
                    }
                }
            }
        }
        Ok(merged)
    }

    /// 利用者辞書を書き出し、手元の状態を書いた内容に合わせる。
    fn write_user(&mut self, merged: HashMap<String, Vec<Candidate>>) -> Result<()> {
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

            writeln!(f, ";; okuri-ari entries.")?;
            for k in keys.iter().filter(|k| is_okuri_ari(k)) {
                write_entry(&mut f, k, &merged[*k])?;
            }
            writeln!(f, ";; okuri-nasi entries.")?;
            for k in keys.iter().filter(|k| !is_okuri_ari(k)) {
                write_entry(&mut f, k, &merged[*k])?;
            }
        }
        fs::rename(&tmp, &self.user_path)?;
        self.changes.clear();
        self.user = merged;
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

fn write_entry(f: &mut fs::File, key: &str, cands: &[Candidate]) -> std::io::Result<()> {
    if cands.is_empty() {
        return Ok(());
    }
    write!(f, "{} /", key)?;
    for c in cands {
        write!(f, "{}/", c.serialize())?;
    }
    writeln!(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_covers_the_circled_numbers() {
        let mut m = HashMap::new();
        load_into(&mut m, BUILTIN);
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
        let (_, c) = parse_line("あ /亜// /唖/").unwrap();
        assert_eq!(
            c.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["亜", "唖"]
        );
        // 全角空白は正当な候補
        let (_, c) = parse_line("すぺーす /　/").unwrap();
        assert_eq!(c[0].text, "　");
    }

    #[test]
    fn completes_by_prefix() {
        let mut sys = HashMap::new();
        load_into(
            &mut sys,
            "かんじ /漢字/\nかんじゃ /患者/\nかんきょう /環境/\nかい /回/\nかんがr /考/\nかん /缶/\n",
        );
        let mut user = HashMap::new();
        load_into(&mut user, "かんきょう /環境/\nかんぱい /乾杯/\n");
        let mut system_sorted: Vec<String> =
            sys.keys().filter(|k| !is_okuri_ari(k)).cloned().collect();
        system_sorted.sort_unstable();
        let d = Dict {
            system: sys,
            user,
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

    #[test]
    fn parses_entries() {
        let (k, c) = parse_line("かんじ /漢字/幹事;幹事さん/").unwrap();
        assert_eq!(k, "かんじ");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "漢字");
        assert_eq!(c[1].text, "幹事");
        assert_eq!(c[1].annotation.as_deref(), Some("幹事さん"));
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
