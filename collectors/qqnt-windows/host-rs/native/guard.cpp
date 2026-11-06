#define wmain qscope_guard_main
#include "../../../../tools/qq-portable/guard.cpp"

extern "C" int qscope_guard(int count, wchar_t** arguments) {
    return qscope_guard_main(count, arguments);
}
