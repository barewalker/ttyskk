/* ttyskk を fcitx5 の入力メソッドとして動かす。
 *
 * 変換そのものは Rust 側 (capi/) が持っていて、ここは fcitx5 との橋渡しに徹する。
 * 設定と辞書は端末の ttyskk と同じ場所を使うので、**端末で覚えた語が GUI でも
 * 先頭に出る**。
 */

#ifndef FCITX5_TTYSKK_ENGINE_H
#define FCITX5_TTYSKK_ENGINE_H

#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>
#include <fcitx/inputcontext.h>
#include <fcitx/inputmethodengine.h>
#include <fcitx/instance.h>

extern "C" {
#include "ttyskk.h"
}

namespace fcitx {

class TtyskkEngine : public InputMethodEngineV2 {
public:
    TtyskkEngine(Instance *instance);
    ~TtyskkEngine() override;

    void keyEvent(const InputMethodEntry &entry, KeyEvent &keyEvent) override;
    void reset(const InputMethodEntry &entry, InputContextEvent &event) override;
    void deactivate(const InputMethodEntry &entry,
                    InputContextEvent &event) override;

    /* 入力メソッドの札に出すモード (あ / ア / 半 / Ａ)。 */
    std::string subMode(const InputMethodEntry &entry,
                        InputContext &ic) override;

private:
    /* 直前の打鍵の結果を画面へ反映する。 */
    void updateUI(InputContext *ic);
    /* 設定ファイルを読み直してエンジンへ渡す。 */
    void reloadConfig() override;

    Instance *instance_;
    /* Rust 側の変換エンジン。作れなければ nullptr で、その場合は何も横取りしない。 */
    ::TtyskkEngine *engine_ = nullptr;
};

class TtyskkEngineFactory : public AddonFactory {
public:
    AddonInstance *create(AddonManager *manager) override {
        return new TtyskkEngine(manager->instance());
    }
};

} // namespace fcitx

#endif
