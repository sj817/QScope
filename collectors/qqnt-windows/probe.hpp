#pragma once
#include <windows.h>
// 在 DllMain 之外、MinHook 初始化之后和 QQ 恢复之前调用。
bool qscope_capture_prepare();
bool qscope_capture_enabled();
void qscope_capture_module_loaded(HMODULE module);
