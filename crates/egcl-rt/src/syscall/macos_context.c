// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/ucontext.h>

// The arm64 thread-state PC may carry pointer authentication. Use the SDK's
// setter instead of writing the saved PC field directly from Rust.
bool egcl_macos_rewrite_ucontext_pc(void *context, uintptr_t pc) {
    if (context == NULL) {
        return false;
    }
    ucontext_t *uc = context;
    if (uc->uc_mcontext == NULL) {
        return false;
    }
    __darwin_arm_thread_state64_set_pc_fptr(
        uc->uc_mcontext->__ss, (void (*)(void))pc);
    return true;
}
