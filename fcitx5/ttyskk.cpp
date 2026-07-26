#include "engine.h"

#include <fcitx-utils/i18n.h>
#include <fcitx/inputpanel.h>

#include <cstdlib>
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

} // namespace

TtyskkEngine::TtyskkEngine(Instance *instance) : instance_(instance) {
    const std::string cfg = readConfig();
    engine_ = ttyskk_new(systemJisyo().c_str(), userJisyo().c_str(),
                         cfg.empty() ? nullptr : cfg.c_str());
}

TtyskkEngine::~TtyskkEngine() {
    if (engine_) {
        /* 覚えたことを書き出してから捨てる。 */
        ttyskk_save(engine_);
        ttyskk_free(engine_);
    }
}

void TtyskkEngine::reloadConfig() {
    if (!engine_) {
        return;
    }
    const std::string cfg = readConfig();
    ttyskk_set_config(engine_, cfg.empty() ? nullptr : cfg.c_str());
}

void TtyskkEngine::keyEvent(const InputMethodEntry &, KeyEvent &keyEvent) {
    if (!engine_ || keyEvent.isRelease()) {
        return;
    }
    auto *ic = keyEvent.inputContext();
    const bool handled =
        ttyskk_key(engine_, static_cast<uint32_t>(keyEvent.key().sym()),
                   keyEvent.key().states().toInteger());

    /* **確定と「呼ぶ側へ委ねる」は同時に起きる。** ▽ の途中で矢印を押すと、
     * 見出し語を確定したうえで矢印は fcitx5 へ渡る。handled が false でも
     * 確定した文字列を見落としてはいけない。 */
    const char *commit = ttyskk_commit(engine_);
    if (commit && *commit) {
        ic->commitString(commit);
    }
    updateUI(ic);

    if (handled) {
        keyEvent.filterAndAccept();
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
    /* 入力メソッドを離れるときは、覚えたことを書き出しておく。
     * fcitx5 は長く動き続けるので、終了時だけでは失われる。 */
    if (engine_) {
        ttyskk_save(engine_);
    }
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
