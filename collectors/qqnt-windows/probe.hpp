#pragma once
#include <windows.h>
// Called outside DllMain, after MinHook initialization and before QQ resumes.
bool qscope_capture_prepare();
bool qscope_capture_enabled();
void qscope_capture_module_loaded(HMODULE module);
