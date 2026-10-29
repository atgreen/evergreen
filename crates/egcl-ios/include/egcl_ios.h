/* SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
 * SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0 */
#ifndef EGCL_IOS_H
#define EGCL_IOS_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
#define EGCL_IOS_ABI_VERSION 1
#define EGCL_IOS_IDLE 0
#define EGCL_IOS_STARTING 1
#define EGCL_IOS_RUNNING 2
#define EGCL_IOS_ROOT_COMPLETED 3
#define EGCL_IOS_STOPPING 4
#define EGCL_IOS_FINISHED 5
#define EGCL_IOS_FAILED 6
struct egcl_ios_config {
    uint32_t abi_version, struct_size;
    const char *source_directory, *writable_directory, *entry_file;
};
struct egcl_ios_status { uint32_t state; int32_t exit_code; };
/* Inputs are copied before return. One accepted start attempt per process. */
int32_t egcl_ios_start(const struct egcl_ios_config *config);
/* Asynchronous: no process signals, no UI-thread joins, no forced kill. */
int32_t egcl_ios_request_stop(void);
int32_t egcl_ios_status(struct egcl_ios_status *out);
size_t egcl_ios_copy_error(char *destination, size_t capacity);
#ifdef __cplusplus
}
#endif
#endif
