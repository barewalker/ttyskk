//! 候補の並べ替えの差し替え口。
//!
//! 既定は画面の文章から手掛かりを探す方式 ([`ScreenHints`])。これを差し替えられるように
//! してあるのは、**手元の言語モデルに点を付けさせる方式を試せるようにする**ためだ。
//!
//! # なぜ候補ごとではなく一括か
//!
//! 一候補ずつ点を付ける形にすると、模型に問い合わせる実装が候補の数だけ推論を回すことに
//! なる。かな漢字変換の模型は「文脈 + 候補の一覧 → どれか」をまとめて答えられるので、
//! **一度の呼び出しで並び全体を返す形**にしておく。遅い相手 (別のプロセス) を後ろに置く
//! 余地も、こちらの形でないと作れない。
//!
//! # 守るべきこと
//!
//! 差し替える実装も、次の二つは守る。既定の実装がそうしているのと同じ理由で、**入れて
//! 悪くなることがない**ようにするためだ。
//!
//! 1. **手掛かりが無ければ並びを変えない** (`0..n` をそのまま返す)
//! 2. **同点のものは元の順を保つ** (元の順は最近使った順。学習を壊さない)
//!
//! 効き方を数で比べる台は `tests/context_eval.rs` にある。`TTYSKK_EVAL_RANKER` で方式を
//! 選べる。

use crate::context::Context;

/// 並べ替えにかける候補。
pub struct Candidate<'a> {
    pub text: &'a str,
    pub annotation: Option<&'a str>,
}

/// 並べ替えの結果。
pub struct Ranking {
    /// 新しい並び (元の添字)。**触らないときは `0..n` をそのまま返す。**
    pub order: Vec<usize>,
    /// 記録 (`TTYSKK_DEBUG`) に出す一行。何がどう効いたかを追えなくなるので、点を付けた
    /// 実装は書き残す。
    pub note: Option<String>,
}

impl Ranking {
    /// 触らない。
    pub fn untouched(len: usize) -> Self {
        Ranking {
            order: (0..len).collect(),
            note: None,
        }
    }
}

/// 画面の文脈で候補を並べ替えるもの。
pub trait Ranker {
    /// 記録と、測る台の表に出す名前。
    fn name(&self) -> &'static str;

    /// `cands` は**並べ替える前の順** (学習が効いた順)。`key` は見出し語で、記録に使う。
    fn rank(&self, ctx: &Context, key: &str, cands: &[Candidate<'_>]) -> Ranking;
}

/// 何もしない。辞書と学習の並びのまま (`context_order = false` と同じ)。
///
/// 測る台で**比べる相手**として使う。
pub struct DictOrder;

impl Ranker for DictOrder {
    fn name(&self) -> &'static str {
        "dict"
    }

    fn rank(&self, _ctx: &Context, _key: &str, cands: &[Candidate<'_>]) -> Ranking {
        Ranking::untouched(cands.len())
    }
}

/// 画面に見えている文章から手掛かりを探す既定の方式。
///
/// 点の付け方は [`Context::explain_at`] にある。
pub struct ScreenHints;

impl Ranker for ScreenHints {
    fn name(&self) -> &'static str {
        "screen"
    }

    fn rank(&self, ctx: &Context, key: &str, cands: &[Candidate<'_>]) -> Ranking {
        let explained: Vec<(f64, Vec<(String, f64)>)> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| ctx.explain_at(i, c.text, c.annotation))
            .collect();
        let scored: Vec<f64> = explained.iter().map(|(s, _)| *s).collect();
        // **手掛かりが無ければ並びを変えない。**
        if scored.iter().all(|s| *s <= 0.0) {
            return Ranking::untouched(cands.len());
        }

        // 点の付いたものを、**効いた語を添えて**重い順に記録へ残す
        let mut note: Vec<(f64, String)> = explained
            .iter()
            .map(|(_, hits)| hits)
            .zip(&scored)
            .zip(cands)
            .filter(|((_, s), _)| **s > 0.0)
            .map(|((hits, s), c)| {
                let why: Vec<String> = hits.iter().take(3).map(|(w, _)| w.clone()).collect();
                (*s, format!("{} {s:.2}[{}]", c.text, why.join(",")))
            })
            .collect();
        note.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let note = format!(
            "{key} → {}\n    画面 {}",
            note.iter()
                .take(5)
                .map(|(_, t)| t.clone())
                .collect::<Vec<_>>()
                .join(" / "),
            ctx.digest(60)
        );

        // 点数の降順。**安定な並べ替え**なので、同点のものは元の順 (最近使った順) を保つ。
        let mut order: Vec<usize> = (0..cands.len()).collect();
        order.sort_by(|a, b| {
            scored[*b]
                .partial_cmp(&scored[*a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ranking {
            order,
            note: Some(note),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands<'a>(v: &'a [(&'a str, Option<&'a str>)]) -> Vec<Candidate<'a>> {
        v.iter()
            .map(|(t, a)| Candidate {
                text: t,
                annotation: *a,
            })
            .collect()
    }

    #[test]
    fn dict_order_never_touches_anything() {
        let ctx = Context::new("校正の話をしている画面", 11);
        let c = cands(&[("構成", None), ("校正", None)]);
        let r = DictOrder.rank(&ctx, "こうせい", &c);
        assert_eq!(r.order, [0, 1], "手掛かりがあっても動かさない");
        assert!(r.note.is_none());
    }

    #[test]
    fn screen_hints_moves_the_mentioned_one_up() {
        let ctx = Context::new("校正の話をしている画面", 11);
        let c = cands(&[("構成", None), ("校正", None)]);
        let r = ScreenHints.rank(&ctx, "こうせい", &c);
        assert_eq!(r.order, [1, 0]);
        // 記録が無いと、狙いと違う候補が出た理由を追えない
        assert!(r.note.as_deref().is_some_and(|s| s.contains("校正")));
    }

    #[test]
    fn screen_hints_leaves_an_unrelated_screen_alone() {
        let ctx = Context::new("$ cargo test --quiet\n   Compiling ttyskk\n", 20);
        let c = cands(&[("構成", None), ("校正", None)]);
        let r = ScreenHints.rank(&ctx, "こうせい", &c);
        assert_eq!(r.order, [0, 1]);
        assert!(r.note.is_none(), "触らないときは記録も残さない");
    }
}
