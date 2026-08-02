#include "engine.h"

#include <fcitx-utils/i18n.h>
#include <fcitx/candidatelist.h>
#include <fcitx/inputpanel.h>

#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>

namespace fcitx {

namespace {

/* 環境変数か、既定の場所。端末側の ttyskk と同じ決まりに揃えてある。 */
std::string envOr(const char *name, const std::string &fallback) {
    const char *v = std::getenv(name);
    return (v && *v) ? std::string(v) : fallback;
}

std::string home() { return envOr("HOME", "."); }

std::string configHome() {
    return envOr("XDG_CONFIG_HOME", home() + "/.config");
}

std::string dataHome() {
    return envOr("XDG_DATA_HOME", home() + "/.local/share");
}

/* 共有辞書の場所。`:` 区切りで、無いものは Rust 側が読み飛ばす。 */
std::string systemJisyo() {
    return envOr("TTYSKK_JISYO", "/usr/share/skk/SKK-JISYO.L");
}

std::string userJisyo() {
    return envOr("TTYSKK_USER_JISYO", dataHome() + "/ttyskk/user.dict");
}

std::string configPath() {
    return envOr("TTYSKK_CONFIG", configHome() + "/ttyskk/config.toml");
}

/* 設定ファイルの中身。無ければ空 (Rust 側が既定で動く)。 */
std::string readConfig() {
    std::ifstream f(configPath());
    if (!f) {
        return {};
    }
    std::ostringstream ss;
    ss << f.rdbuf();
    return ss.str();
}

/* 書き方を調べる先。設定画面の釦から開く。 */
constexpr char REPOSITORY[] = "https://github.com/barewalker/ttyskk";

/* `QProcess::splitCommand` に一語として渡るよう包む。空白を含むパスのため。 */
std::string quoted(const std::string &s) {
    std::string out = "\"";
    for (const char c : s) {
        if (c == '"' || c == '\\') {
            out += '\\';
        }
        out += c;
    }
    out += '"';
    return out;
}

/* 設定画面の釦が起こすもの。
 *
 * **fcitx5-configtool は `fcitx://` で始まらない値をコマンド行として実行する**
 * (fcitx5-configtool の `launchExternalConfig`)。専用の GUI を書かなくても、
 * 利用者がふだん使っている編集器を開ける。 */
std::string openCommand(const std::string &target) {
    return "xdg-open " + quoted(target);
}

/* 設定ファイルが無ければ、案内だけ書いたものを置く。
 *
 * 中身が全部注釈なので動きは変わらない。**開く相手が無いと釦を押しても何も
 * 起きない**ので、画面を出す前に必ず在ることにしておく。 */
void ensureConfigFile() {
    namespace fs = std::filesystem;
    const fs::path path(configPath());
    std::error_code ec;
    if (fs::exists(path, ec)) {
        return;
    }
    fs::create_directories(path.parent_path(), ec);
    std::ofstream out(path);
    if (!out) {
        return;
    }
    out << "# ttyskk の設定。**端末の ttyskk と共通**で、fcitx5 側には別に持たない。\n"
           "#\n"
           "# 設定できる項目を全部並べた雛形は `ttyskk --config-example` で書き出せる。\n"
           "# 書き方: "
        << REPOSITORY << "\n";
}

} // namespace

/* 設定画面 (歯車) に出すもの。
 *
 * **設定項目そのものは置かない。** 端末側と同じ `config.toml` を読むので、fcitx5 に
 * も並べると同じ項目の置き場所が二つになる。ここにあるのは「どこを書き換えるか」の
 * 案内と、その場所を開く釦だけ。
 *
 * **行が二つあるのには理由がある。** fcitx5-configtool は `External` が一つだけの
 * 設定を「画面ではなく、押した瞬間に起こすもの」と見なして画面を出さない
 * (`extractOnlyExternalCommand`)。案内を読ませたいので二つ置く。 */
FCITX_CONFIGURATION(
    TtyskkConfig,
    ExternalOption configFile{
        this, "ConfigFile",
        "設定は端末の ttyskk と同じ " + configPath() +
            " に書く (書き換えて OK を押すと読み直す)",
        openCommand(configPath())};
    ExternalOption document{
        this, "Document",
        "書き方は README にある。全項目の雛形は ttyskk --config-example で書き出せる",
        openCommand(REPOSITORY)};);

TtyskkEngine::TtyskkEngine(Instance *instance)
    : instance_(instance), config_(std::make_unique<TtyskkConfig>()) {
    const std::string cfg = readConfig();
    engine_ = ttyskk_new(systemJisyo().c_str(), userJisyo().c_str(),
                         cfg.empty() ? nullptr : cfg.c_str());
}

TtyskkEngine::~TtyskkEngine() {
    if (engine_) {
        /* 覚えたことを書き出してから捨てる。 */
        saveNow();
        ttyskk_free(engine_);
    }
}

/* 覚えたことを書き出すまでの待ち。
 *
 * 確定のたびに書くとディスクを叩きすぎる。逆に入力メソッドを離れるまで待つと、
 * 端末の ttyskk から見えるようになるのが遅すぎる (窓ごとに入力コンテキストが
 * 独立しているので、別の窓で切り替えても離れたことにならない)。 */
static constexpr uint64_t SAVE_DELAY_USEC = 3 * 1000 * 1000;

void TtyskkEngine::scheduleSave() {
    if (!engine_ || saveTimer_) {
        return; /* すでに予約してある */
    }
    saveTimer_ = instance_->eventLoop().addTimeEvent(
        CLOCK_MONOTONIC, now(CLOCK_MONOTONIC) + SAVE_DELAY_USEC, 0,
        [this](EventSourceTime *, uint64_t) {
            if (engine_) {
                ttyskk_save(engine_);
            }
            saveTimer_.reset();
            return true;
        });
}

void TtyskkEngine::saveNow() {
    saveTimer_.reset();
    if (engine_) {
        ttyskk_save(engine_);
    }
}

void TtyskkEngine::reloadConfig() {
    if (!engine_) {
        return;
    }
    const std::string cfg = readConfig();
    ttyskk_set_config(engine_, cfg.empty() ? nullptr : cfg.c_str());
}

const Configuration *TtyskkEngine::getConfig() const {
    /* 開く相手を用意してから見せる。押しても何も起きない釦を出さないため。 */
    ensureConfigFile();
    return config_.get();
}

void TtyskkEngine::setConfig(const RawConfig &) {
    /* **fcitx5 側に持つ値は無い。** 画面から編集器を開いて書き換えたあと、そのまま
     * OK で反映されるように、ここを設定ファイルを読み直す口として使う。 */
    reloadConfig();
}

void TtyskkEngine::keyEvent(const InputMethodEntry &, KeyEvent &keyEvent) {
    if (!engine_ || keyEvent.isRelease()) {
        return;
    }
    auto *ic = keyEvent.inputContext();
    updateContext(ic);
    const bool handled =
        ttyskk_key(engine_, static_cast<uint32_t>(keyEvent.key().sym()),
                   keyEvent.key().states().toInteger());

    /* **確定と「呼ぶ側へ委ねる」は同時に起きる。** ▽ の途中で矢印を押すと、
     * 見出し語を確定したうえで矢印は fcitx5 へ渡る。handled が false でも
     * 確定した文字列を見落としてはいけない。 */
    const char *commit = ttyskk_commit(engine_);
    if (commit && *commit) {
        ic->commitString(commit);
        /* 確定したということは何か覚えた見込みがある。少し置いて書き出す。 */
        scheduleSave();
    }
    updateUI(ic);

    if (handled) {
        keyEvent.filterAndAccept();
    }
}

/* 入力欄に見えている文章を文脈として渡す。同音異義語の順序に効く。
 *
 * 端末の ttyskk は子アプリの画面の控えを持っていて、そこから組んで渡している。GUI で
 * それにあたるのが **fcitx5 の周辺テキスト**で、カーソルはどちらも文字数で数えるので
 * (`SurroundingText::cursor()` は "offset of cursor in character") そのまま渡せる。
 *
 * **周辺テキストは子アプリが送ってこなければ無い。** GTK/Qt の入力欄は概ね送ってくる
 * が、端末エミュレータや一部のアプリは持たない。無いときは**忘れさせる** — エンジンは
 * 窓ごとに分かれていないので、渡しっぱなしにすると前の窓の話題で並べ替えてしまう。
 *
 * 打鍵のたびに渡している。端末側は画面を組み直すのが高くつくので変化を見ているが、
 * ここでは fcitx5 が持っている文字列を写すだけなので、見張る仕掛けに見合わない。 */
void TtyskkEngine::updateContext(InputContext *ic) {
    if (!ttyskk_wants_context(engine_)) {
        return; /* 設定で切ってあれば、組み立てること自体をしない */
    }
    const SurroundingText &surrounding = ic->surroundingText();
    if (surrounding.isValid()) {
        ttyskk_set_context(engine_, surrounding.text().c_str(),
                           surrounding.cursor());
    } else {
        ttyskk_set_context(engine_, nullptr, 0);
    }
}

void TtyskkEngine::updateUI(InputContext *ic) {
    Text preedit;
    const size_t n = ttyskk_preedit_len(engine_);
    for (size_t i = 0; i < n; i++) {
        TextFormatFlags fmt = TextFormatFlag::Underline;
        if (ttyskk_preedit_style(engine_, i) == TTYSKK_STYLE_CANDIDATE) {
            fmt = TextFormatFlag::HighLight;
        }
        preedit.append(std::string(ttyskk_preedit_text(engine_, i)), fmt);
    }
    if (n > 0) {
        preedit.setCursor(preedit.textLength());
    }

    ic->inputPanel().reset();
    ic->inputPanel().setClientPreedit(preedit);

    /* 候補窓。**候補があること**と**一覧を出すこと**は別で、SKK では最初の数件を
     * 一つずつ送り、それを過ぎたところで一覧に切り替える。判断は Rust 側が持つ。
     *
     * 選ぶ操作 (a s d f …) はエンジンが自分で扱うので、ここは表示だけを担う。 */
    if (ttyskk_candidate_visible(engine_)) {
        auto list = std::make_unique<CommonCandidateList>();
        list->setLayoutHint(CandidateLayoutHint::Horizontal);

        const std::string labels(ttyskk_candidate_labels(engine_));
        std::vector<std::string> shown;
        shown.reserve(labels.size());
        for (const char c : labels) {
            shown.push_back(std::string(1, c) + ": ");
        }
        if (!shown.empty()) {
            list->setPageSize(static_cast<int>(shown.size()));
            list->setLabels(shown);
        }

        const size_t n = ttyskk_candidate_len(engine_);
        for (size_t i = 0; i < n; i++) {
            Text text(std::string(ttyskk_candidate_text(engine_, i)));
            const char *annot = ttyskk_candidate_annotation(engine_, i);
            if (annot && *annot) {
                text.append(" ; " + std::string(annot));
            }
            list->append<DisplayOnlyCandidateWord>(std::move(text));
        }
        list->setGlobalCursorIndex(
            static_cast<int>(ttyskk_candidate_selected(engine_)));
        ic->inputPanel().setCandidateList(std::move(list));
    }

    ic->updatePreedit();
    ic->updateUserInterface(UserInterfaceComponent::InputPanel);
}

void TtyskkEngine::reset(const InputMethodEntry &, InputContextEvent &event) {
    if (!engine_) {
        return;
    }
    ttyskk_reset(engine_);
    updateUI(event.inputContext());
}

void TtyskkEngine::deactivate(const InputMethodEntry &entry,
                              InputContextEvent &event) {
    /* 入力メソッドを離れるときは、待たずに書き出す。 */
    saveNow();
    reset(entry, event);
}

std::string TtyskkEngine::subMode(const InputMethodEntry &, InputContext &) {
    if (!engine_) {
        return {};
    }
    switch (ttyskk_mode(engine_)) {
    case TTYSKK_MODE_HIRAGANA:
        return "あ";
    case TTYSKK_MODE_KATAKANA:
        return "ア";
    case TTYSKK_MODE_HANKAKU_KATAKANA:
        return "半";
    case TTYSKK_MODE_ZENKAKU_ASCII:
        return "Ａ";
    default:
        return "A";
    }
}

} // namespace fcitx

FCITX_ADDON_FACTORY(fcitx::TtyskkEngineFactory)
