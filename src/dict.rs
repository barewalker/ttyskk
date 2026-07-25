//! SKK 辞書の読み込みと引き当て。
//!
//! 見出し語は送りなしが「かな」、送りありが「かな + 送り仮名のローマ字頭文字」
//! (例: 「動く」なら `うごk`)。この二つは同じ表に入れても衝突しないため、
//! ひとつの HashMap で扱う。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// この起動で覚えたことを、起きた順に記録する。
    ///
    /// 丸ごと書き出すと、herdr で複数のペインを開いているときに互いの学習を
    /// 消し合ってしまう。保存時にディスクの内容を読み直し、この記録だけを
    /// 重ねることで、他のペインの学習を残したまま書ける。
    learned: Vec<(String, Candidate)>,
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

        Ok(Dict {
            system,
            user,
            user_path,
            import_path,
            learned: Vec::new(),
        })
    }

    /// 共有辞書の見出し語数。
    pub fn system_len(&self) -> usize {
        self.system.len()
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
        let pick = |map: &HashMap<String, Vec<Candidate>>| {
            let mut v: Vec<String> = map
                .keys()
                .filter(|k| k.len() > prefix.len() && k.starts_with(prefix) && !is_okuri_ari(k))
                .cloned()
                .collect();
            v.sort_by(|a, b| a.chars().count().cmp(&b.chars().count()).then(a.cmp(b)));
            v
        };
        let mut out = pick(&self.user);
        out.truncate(limit);
        for k in pick(&self.system) {
            if out.len() >= limit {
                break;
            }
            if !out.contains(&k) {
                out.push(k);
            }
        }
        out
    }

    /// 確定した候補を利用者辞書の先頭に移す (学習)。
    pub fn learn(&mut self, key: &str, cand: &Candidate) {
        move_to_front(self.user.entry(key.to_string()).or_default(), cand);
        self.learned.push((key.to_string(), cand.clone()));
    }

    /// 利用者辞書を書き出す。覚えたことがなければ何もしない。
    pub fn save(&mut self) -> Result<()> {
        if self.learned.is_empty() {
            return Ok(());
        }
        // ディスクの現状を読み直してから、この起動で覚えたことを重ねる
        let mut merged = HashMap::new();
        if self.user_path.exists() {
            load_into(&mut merged, &read_jisyo(&self.user_path)?);
        } else if let Some(src) = &self.import_path
            && src.exists()
        {
            load_into(&mut merged, &read_jisyo(src)?);
        }
        for (key, cand) in &self.learned {
            move_to_front(merged.entry(key.clone()).or_default(), cand);
        }

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
        self.learned.clear();
        Ok(())
    }
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
        let d = Dict {
            system: sys,
            user,
            user_path: PathBuf::from("/dev/null"),
            import_path: None,
            learned: Vec::new(),
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
