# 统一编译选项，供宿主与所有进程内插件共用。
#
# 依据 docs/ling/05-构造与开发指南.md §3.2：
#   编译器、标准版本、异常模型、运行时库在任何一处不一致，
#   跨动态库传递对象就会出现「能链接、运行崩溃」，所以这三个函数是唯一入口，
#   任何目标都不许自己写 target_compile_options。
#
# Debug 与 Release 只差三处（代码生成等级、调试信息、NDEBUG），由 CMake 的默认
# 变量 CMAKE_CXX_FLAGS_<CONFIG> 负责，本文件不重复设置。

# C++17、异常模型、运行时库。MSVC 的 /MD 与 /MT 不能混用，
# 用 CMAKE_MSVC_RUNTIME_LIBRARY 统一成动态 CRT。
function(ling_set_standard target)
    target_compile_features(${target} PUBLIC cxx_std_17)

    if(MSVC)
        # /utf-8：源码与执行字符集都按 UTF-8。缺了它会按本地代码页（中文 Windows 是 GBK）
        # 读源文件，中文注释的尾字节可能正好是 0x5C（反斜杠），把下一行续成注释，代码被吞掉。
        target_compile_options(${target} PUBLIC /std:c++17 /EHsc /utf-8)
        set_property(TARGET ${target} PROPERTY MSVC_RUNTIME_LIBRARY
                     "MultiThreaded$<$<CONFIG:Debug>:Debug>DLL")
    else()
        target_compile_options(${target} PUBLIC -std=c++17)
    endif()
endfunction()

# 警告全集并视为错误。故意写一个未使用的变量必须让构建失败，这正是 S0 的验收方式。
function(ling_set_warnings target)
    if(MSVC)
        target_compile_options(${target} PRIVATE /W4 /WX)
    else()
        target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -Werror)
    endif()
endfunction()

# 崩溃时可定位：保留帧指针，加栈保护。Release 也要开，
# 否则线上崩溃只能拿到地址，拿不到调用栈。
function(ling_set_hardening target)
    if(MSVC)
        target_compile_options(${target} PRIVATE /Oy- /GS)
    else()
        target_compile_options(${target} PRIVATE -fno-omit-frame-pointer
                                                  -fstack-protector-strong)
    endif()
endfunction()

# 消毒器，只在 Debug 开（docs/ling/05-构造与开发指南.md §4.2）。
# Windows 的 MSVC 只有 ASan，没有 UBSan；Windows 上的未定义行为靠 Linux 侧同名测试兜住。
function(ling_set_sanitizer target)
    if(NOT LING_SANITIZER)
        return()
    endif()

    if(MSVC)
        target_compile_options(${target} PRIVATE /fsanitize=address)
        target_link_options(${target} PRIVATE /INCREMENTAL:NO)
    else()
        target_compile_options(${target} PRIVATE -fsanitize=address,undefined
                                                  -fno-sanitize-recover=all)
        target_link_options(${target} PRIVATE -fsanitize=address,undefined)
    endif()
endfunction()
