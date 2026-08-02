/* ttyskk の変換エンジンを C から使うための宣言。
 *
 * このファイルは cbindgen が capi/src/lib.rs から作る。手で直さない。
 * 作り直すには capi の中で cargo build。
 */

#ifndef TTYSKK_H
#define TTYSKK_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * 入力モード。`ttyskk_mode` が返す。
 */
#define TTYSKK_MODE_ASCII 0

#define TTYSKK_MODE_HIRAGANA 1

#define TTYSKK_MODE_KATAKANA 2

#define TTYSKK_MODE_HANKAKU_KATAKANA 3

#define TTYSKK_MODE_ZENKAKU_ASCII 4

/**
 * 入力中の表示の装飾。`ttyskk_preedit_style` が返す。
 */
#define TTYSKK_STYLE_READING 0

#define TTYSKK_STYLE_ROMAJI 1

#define TTYSKK_STYLE_CANDIDATE 2

/**
 * 見出し語の中でカーソルが乗っている一文字。入力メソッド側で位置を示すのに使う。
 */
#define TTYSKK_STYLE_READING_CURSOR 3

/**
 * 打つそばから見せている補完。**まだ打っていない文字**なので、打った分と
 * 見分けの付く見た目にする (端末では薄字)。
 */
#define TTYSKK_STYLE_COMPLETION 4

/**
 * 変換エンジンひとつ分。C 側からは不透明な入れ物として扱う。
 */
typedef struct TtyskkEngine TtyskkEngine;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * エンジンを作る。作れなければ NULL。
 *
 * - `system_jisyo_paths` … 共有辞書のパスを `:` で繋いだもの。NULL なら共有辞書なし
 * - `user_jisyo_path` … 利用者辞書のパス。学習はここへ書き戻す
 * - `config_toml` … 設定ファイルの中身そのもの。NULL なら既定
 *
 * # Safety
 * 引数はいずれも NULL か、NUL で終わる有効な文字列を指していること。
 */
struct TtyskkEngine *ttyskk_new(const char *system_jisyo_paths,
                                const char *user_jisyo_path,
                                const char *config_toml);

/**
 * エンジンを捨てる。学習は書き出さないので、必要なら先に [`ttyskk_save`]。
 *
 * # Safety
 * `p` は [`ttyskk_new`] が返したもので、まだ捨てていないこと。
 */
void ttyskk_free(struct TtyskkEngine *p);

/**
 * 設定を入れ替える。読めなければ何もしない。
 *
 * # Safety
 * `p` は有効なハンドル、`config_toml` は NULL か有効な文字列。
 */
void ttyskk_set_config(struct TtyskkEngine *p, const char *config_toml);

/**
 * 学習を利用者辞書へ書き出す。
 *
 * ディスクの現状を読み直してから重ねるので、端末側の ttyskk が同時に動いていても
 * 互いの学習を消さない。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
void ttyskk_save(struct TtyskkEngine *p);

/**
 * 入力中の内容をすべて捨てる。モードは変えない。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
void ttyskk_reset(struct TtyskkEngine *p);

/**
 * 文脈を渡す意味があるか。
 *
 * 設定 (`[behavior] context_order`) が無効なら false。**周辺テキストを組み立てる前に
 * これを見る** — 呼ぶ側で毎打鍵ごとに文字列を作る手間を省ける。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
bool ttyskk_wants_context(const struct TtyskkEngine *p);

/**
 * 入力欄に見えている文章を文脈として渡す。同音異義語の順序に効く。
 *
 * - `text` … UTF-8 の文字列。NULL か空なら**文脈を忘れる** (順序を変えなくなる)
 * - `cursor` … カーソルの位置。**バイト数ではなく文字数**で数える。fcitx5 の
 *   `SurroundingText::cursor()` はこの単位なのでそのまま渡してよい。長すぎる値は
 *   末尾に丸める
 *
 * 渡した文脈は次に渡し直すまで残る。**入力欄が周辺テキストを持たない場に移ったら、
 * 空を渡して忘れさせること** — 前の窓の話題が残ったまま並べ替えてしまう。
 *
 * # Safety
 * `p` は有効なハンドル。`text` は NULL か、NUL で終わる有効な文字列。
 */
void ttyskk_set_context(struct TtyskkEngine *p,
                        const char *text,
                        uintptr_t cursor);

/**
 * キーを一つ渡す。**エンジンが受け取ったなら true**。
 *
 * false のときは呼ぶ側がそのキーを自分で扱う (矢印や機能キー、ASCII モードの打鍵)。
 * **false でも確定した文字列が出ていることがある** — ▽ の途中で矢印を押すと、見出し語
 * を確定したうえで矢印は呼ぶ側へ渡る。[`ttyskk_commit`] は必ず見ること。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
bool ttyskk_key(struct TtyskkEngine *p,
                uint32_t keysym,
                uint32_t modifiers);

/**
 * 直前の打鍵で確定した文字列。確定していなければ空文字列。
 *
 * # Safety
 * `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
 */
const char *ttyskk_commit(const struct TtyskkEngine *p);

/**
 * いまの入力モード (`TTYSKK_MODE_*`)。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
int32_t ttyskk_mode(const struct TtyskkEngine *p);

/**
 * 入力中の表示の区間数。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
uintptr_t ttyskk_preedit_len(const struct TtyskkEngine *p);

/**
 * `i` 番目の区間の文字列。範囲外なら空文字列。
 *
 * # Safety
 * `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
 */
const char *ttyskk_preedit_text(const struct TtyskkEngine *p,
                                uintptr_t i);

/**
 * `i` 番目の区間の装飾 (`TTYSKK_STYLE_*`)。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
int32_t ttyskk_preedit_style(const struct TtyskkEngine *p, uintptr_t i);

/**
 * 候補の数。選択中 (▼) でなければ 0。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
uintptr_t ttyskk_candidate_len(const struct TtyskkEngine *p);

/**
 * `i` 番目の候補。範囲外なら空文字列。
 *
 * # Safety
 * `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
 */
const char *ttyskk_candidate_text(const struct TtyskkEngine *p,
                                  uintptr_t i);

/**
 * `i` 番目の候補の注釈 (辞書の `;` 以降)。無ければ空文字列。
 *
 * # Safety
 * `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
 */
const char *ttyskk_candidate_annotation(const struct TtyskkEngine *p,
                                        uintptr_t i);

/**
 * 選ばれている候補の位置。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
uintptr_t ttyskk_candidate_selected(const struct TtyskkEngine *p);

/**
 * 候補の一覧を出す段階か。
 *
 * SKK では最初の数件を一つずつ送り、それを過ぎたところで一覧に切り替える習わし
 * (何件目からかは設定の `candidates.inline`)。**候補があること**と**一覧を出すこと**
 * は別なので、窓を出すかどうかはこちらで判断する。
 *
 * # Safety
 * `p` は有効なハンドル。
 */
bool ttyskk_candidate_visible(const struct TtyskkEngine *p);

/**
 * 候補一覧から選ぶキーを並べたもの ("asdfjkl" など)。文字数が一頁の大きさになる。
 *
 * # Safety
 * `p` は有効なハンドル。返ったポインタは次に同じハンドルを触るまで有効。
 */
const char *ttyskk_candidate_labels(const struct TtyskkEngine *p);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* TTYSKK_H */
