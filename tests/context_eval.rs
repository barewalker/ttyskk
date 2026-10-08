//! 画面の文脈による並べ替えの実力を数字にする台。
//!
//! 測るのは一つだけ。**正解の候補が何番目に出るか** — SKK では空白を押し直す回数
//! そのものなので、使う側の損がそのまま出る。
//!
//! ```sh
//! cargo test --test context_eval -- --nocapture
//! ```
//!
//! # `context_sample.rs` との違い
//!
//! あちらは**当たり外れの見張り**で、6 件が一枚の長い文書に同居している (節をまたいで
//! 語が互いに影響する、実際の画面に近い形)。こちらは**点数の付け方を比べる台**で、
//! 1 件ずつ切り離して順位を数える。役割が違うので両方残すが、**文章は流用しない**。
//! 片方を直したときにもう片方が黙って食い違わないよう、問題は `tests/data/context-eval.toml`
//! の一箇所にだけ置く (このファイルに答えを書き写さない)。
//!
//! # 落ちる条件
//!
//! 正解率は**数字の報告に留める** (辞書の版で上下するものを合否にすると、辞書を更新した
//! 人が困る)。試験として落ちるのは次の三つだけ。
//!
//! - `control` 群で並びが辞書順と違った (手掛かりが無いのに触った)
//! - `trap` / `reported` 群で、`forbid` の候補が辞書順より順位を上げた
//! - 問題集の作り方が崩れた (`pair` の文章に正解の表記が入った、測れた件数が少なすぎる)

use std::collections::BTreeMap;
use std::path::PathBuf;

use ttyskk::{
    context::{self, Context},
    dict::{Candidate, Dict},
};

/// 共有辞書。無ければ何も測れないので、断って通す。
fn system_jisyo() -> Option<PathBuf> {
    [
        "/usr/share/skk/SKK-JISYO.L",
        "/run/host/usr/share/skk/SKK-JISYO.L",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

struct Item {
    id: String,
    kind: String,
    pair: Option<String>,
    reading: String,
    expect: Option<String>,
    forbid: Vec<String>,
    context: String,
}

fn load_items() -> Vec<Item> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/context-eval.toml"
    ))
    .expect("問題集を読めない");
    let table: toml::Table = text.parse().expect("問題集の TOML が壊れている");
    let items = table
        .get("item")
        .and_then(|v| v.as_array())
        .expect("[[item]] が無い");

    items
        .iter()
        .map(|v| {
            let get = |k: &str| v.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let id = get("id").expect("id が無い");
            Item {
                kind: get("kind").unwrap_or_else(|| panic!("{id}: kind が無い")),
                pair: get("pair"),
                reading: get("reading").unwrap_or_else(|| panic!("{id}: reading が無い")),
                expect: get("expect"),
                forbid: v
                    .get("forbid")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                // **前後の空白を落とした末尾がカーソル位置。**
                context: get("context").unwrap_or_default().trim().to_string(),
                id,
            }
        })
        .collect()
}

/// `skk.rs` の `sort_by_context` と同じ並べ替えを、辞書の候補に対して行う。
///
/// **本番と同じにしておくこと。** 点が全部 0 なら触らない、同点は辞書の順のまま
/// (安定な並べ替え)、いまの並びでの位置を罰として引く、の三点が合否をそのまま決める。
fn reorder(ctx: &Context, cands: &[Candidate]) -> Vec<usize> {
    let order: Vec<usize> = (0..cands.len()).collect();
    if ctx.is_empty() || cands.len() < 2 {
        return order;
    }
    let scored: Vec<f64> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            context::with_rank_penalty(ctx.score(&c.text, c.annotation.as_deref()), i)
        })
        .collect();
    if scored.iter().all(|s| *s <= 0.0) {
        return order;
    }
    let mut order = order;
    order.sort_by(|a, b| {
        scored[*b]
            .partial_cmp(&scored[*a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

/// 並びの中で何番目か (1 から数える)。無ければ `None`。
fn rank(order: &[usize], cands: &[Candidate], text: &str) -> Option<usize> {
    order
        .iter()
        .position(|i| cands[*i].text == text)
        .map(|p| p + 1)
}

#[derive(Default)]
struct Tally {
    items: usize,
    measured: usize,
    top1_dict: usize,
    top1_ctx: usize,
    keys_dict: usize,
    keys_ctx: usize,
    worse: usize,
    untouched: usize,
}

#[test]
fn measure_context_order() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    // **利用者辞書は使わない。** 走らせる人の学習に左右されると基準にならない。
    let dict = Dict::load(&[jisyo], PathBuf::from("/nonexistent/user.dict"), None)
        .expect("辞書を読めない");
    let items = load_items();

    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    let mut pairs: BTreeMap<String, Vec<(bool, bool)>> = BTreeMap::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut broke_control: Vec<String> = Vec::new();
    let mut promoted_forbidden: Vec<String> = Vec::new();
    let mut leaked_answer: Vec<String> = Vec::new();
    let mut reported: Vec<String> = Vec::new();
    let mut measured_total = 0usize;

    for it in &items {
        // 作り方の見張り: `pair` の文章には正解の表記を書かない (難しさが消える)。
        // `surface` は見えていること自体が試すところで、`reported` は実際の画面を
        // そのまま写したものなので、どちらも対象にしない。
        if it.kind == "pair"
            && let Some(e) = &it.expect
            && it.context.contains(e)
        {
            leaked_answer.push(format!("{} ({e} が文章に入っている)", it.id));
        }

        let cands = dict.lookup(&it.reading);
        let ctx = Context::new(&it.context, it.context.chars().count());
        let dict_order: Vec<usize> = (0..cands.len()).collect();
        let ctx_order = reorder(&ctx, &cands);

        let t = tally.entry(it.kind.clone()).or_default();
        t.items += 1;
        if ctx_order == dict_order {
            t.untouched += 1;
        }

        // 対照群は「触らない」ことが唯一の要求。
        if it.kind == "control" && ctx_order != dict_order {
            let moved: Vec<&str> = ctx_order
                .iter()
                .take(3)
                .map(|i| cands[*i].text.as_str())
                .collect();
            broke_control.push(format!("{} → {moved:?}", it.id));
        }

        // 偽の手掛かりで順位を上げてはいけない候補。
        // **`reported` 群はここでは見ない** — 下の `reported_cases` で別に落とす。
        for f in it.forbid.iter().filter(|_| it.kind != "reported") {
            if let (Some(before), Some(after)) =
                (rank(&dict_order, &cands, f), rank(&ctx_order, &cands, f))
                && after < before
            {
                promoted_forbidden.push(format!("{}: {f} が {before} 番目 → {after} 番目", it.id));
            }
        }

        let Some(expect) = &it.expect else { continue };
        let (Some(r_dict), Some(r_ctx)) = (
            rank(&dict_order, &cands, expect),
            rank(&ctx_order, &cands, expect),
        ) else {
            skipped.push(format!("{} ({expect} が辞書に無い)", it.id));
            continue;
        };
        t.measured += 1;
        measured_total += 1;
        t.top1_dict += usize::from(r_dict == 1);
        t.top1_ctx += usize::from(r_ctx == 1);
        t.keys_dict += r_dict - 1;
        t.keys_ctx += r_ctx - 1;
        if r_ctx > r_dict {
            t.worse += 1;
        }
        if let Some(p) = &it.pair {
            pairs
                .entry(p.clone())
                .or_default()
                .push((r_dict == 1, r_ctx == 1));
        }
        if it.kind == "reported" {
            // 実際に外した例は、何が効いたかまで出す (直し方を決めるため)。
            let hits: Vec<String> = ctx_order
                .iter()
                .take(3)
                .map(|i| {
                    let (s, why) = ctx.explain(&cands[*i].text, cands[*i].annotation.as_deref());
                    let why: Vec<&str> = why.iter().take(3).map(|(w, _)| w.as_str()).collect();
                    format!("{} {s:.2}{why:?}", cands[*i].text)
                })
                .collect();
            reported.push(format!(
                "  {}: {expect} は {r_dict} 番目 → {r_ctx} 番目 / 上位 {}",
                it.id,
                hits.join(" ")
            ));
        }
    }

    println!("\n== 群ごと ==");
    println!(
        "{:<14} {:>4} {:>4} {:>9} {:>9} {:>9} {:>9} {:>5} {:>5}",
        "群",
        "件数",
        "測定",
        "1位(辞書)",
        "1位(文脈)",
        "打鍵(辞書)",
        "打鍵(文脈)",
        "悪化",
        "無変化"
    );
    for (kind, t) in &tally {
        println!(
            "{:<14} {:>4} {:>4} {:>9} {:>9} {:>9} {:>9} {:>5} {:>5}",
            kind,
            t.items,
            t.measured,
            t.top1_dict,
            t.top1_ctx,
            t.keys_dict,
            t.keys_ctx,
            t.worse,
            t.untouched
        );
    }

    let both_dict = pairs.values().filter(|v| v.iter().all(|(d, _)| *d)).count();
    let both_ctx = pairs.values().filter(|v| v.iter().all(|(_, c)| *c)).count();
    println!(
        "\n== 対 ==\n組 {} / 両方 1 位になった組: 辞書順 {} → 文脈 {}",
        pairs.len(),
        both_dict,
        both_ctx
    );

    if !reported.is_empty() {
        println!("\n== 実際に外した例 ==");
        for line in &reported {
            println!("{line}");
        }
    }
    if !skipped.is_empty() {
        println!("\n== 除外 ({} 件) ==", skipped.len());
        for s in &skipped {
            println!("  {s}");
        }
    }

    assert!(
        leaked_answer.is_empty(),
        "正解の表記が文章に入っている (surface 群以外では禁止): {leaked_answer:#?}"
    );
    assert!(
        broke_control.is_empty(),
        "手掛かりが無いのに並びを変えた: {broke_control:#?}"
    );
    assert!(
        promoted_forbidden.is_empty(),
        "偽の手掛かりで順位を上げた: {promoted_forbidden:#?}"
    );
    // **空回りの歯止め。** 辞書の版や項目の削除で測れる件数が減ったら、数字が良く
    // 見えても意味が無い。
    assert!(
        measured_total >= 30,
        "測れた件数が少なすぎる ({measured_total} 件)。辞書か問題集を確かめる"
    );
}

/// 使っていて頻発した取りこぼし。**いまは落ちる。**
///
/// 「いち」で位置を出したいのに、画面の数字に引かれて辞書の候補 `1` が先頭に来る。
/// 「あん」で案を出したいのに、助数詞として画面に散らばる「行」が先頭に来る。
/// どちらも**一文字 (または数字) の候補が、端末ならどこにでもある字と当たっている**。
///
/// 直したらこの試験が通る。通ったら `measure_context_order` の打鍵の数も下がる。
#[test]
fn reported_cases() {
    let Some(jisyo) = system_jisyo() else {
        eprintln!("共有辞書が無いので飛ばす");
        return;
    };
    let dict = Dict::load(&[jisyo], PathBuf::from("/nonexistent/user.dict"), None)
        .expect("辞書を読めない");

    let mut bad: Vec<String> = Vec::new();
    for it in load_items().iter().filter(|it| it.kind == "reported") {
        let cands = dict.lookup(&it.reading);
        let ctx = Context::new(&it.context, it.context.chars().count());
        let dict_order: Vec<usize> = (0..cands.len()).collect();
        let ctx_order = reorder(&ctx, &cands);
        let expect = it.expect.as_deref().expect("reported には expect が要る");

        // **求めるのは「文脈のせいで下がらないこと」**。先頭に出すことは求めない —
        // 画面に手掛かりが無ければ上げようがなく (「位置」は辞書順で 2 番目、「案」は
        // 1 番目)、そこを埋めるのは学習か、別の点数の付け方の仕事になる。
        match (
            rank(&dict_order, &cands, expect),
            rank(&ctx_order, &cands, expect),
        ) {
            (Some(before), Some(after)) if after > before => {
                bad.push(format!(
                    "{}: {expect} が {before} 番目 → {after} 番目",
                    it.id
                ));
            }
            (None, _) | (_, None) => bad.push(format!("{}: {expect} が辞書に無い", it.id)),
            _ => {}
        }
        // 画面のどこにでもある字で順位を上げていないこと。
        for f in &it.forbid {
            if let (Some(before), Some(after)) =
                (rank(&dict_order, &cands, f), rank(&ctx_order, &cands, f))
                && after < before
            {
                bad.push(format!("{}: {f} が {before} 番目 → {after} 番目", it.id));
            }
        }
    }
    assert!(bad.is_empty(), "報告された取りこぼし: {bad:#?}");
}
