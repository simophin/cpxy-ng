/* Generated with: cbindgen --config client/cbindgen.toml --crate client --output client/include/cpxy_client.h */

#ifndef CPXY_CLIENT_H
#define CPXY_CLIENT_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#define CPXY_CLIENT_ABI_VERSION 1

typedef void *cpxy_client_handle;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

uint32_t cpxy_client_abi_version(void);

/**
 * Creates a client session.
 *
 * All non-null string pointers must refer to readable, NUL-terminated C strings. `dns_server`
 * and `main_server_url` are required; the remaining URL pointers are optional. When non-null,
 * `error_buffer` must point to writable storage of at least `error_buffer_capacity` bytes.
 *
 * # Safety
 *
 * The caller must uphold the pointer validity requirements above for the duration of this call.
 */
cpxy_client_handle cpxy_client_create(uint16_t http_proxy_port,
                                      uint16_t socks5_proxy_port,
                                      uint16_t api_proxy_port,
                                      const char *dns_server,
                                      const char *main_server_url,
                                      const char *ai_server_url,
                                      const char *tailscale_server_url,
                                      char *error_buffer,
                                      uint32_t error_buffer_capacity);

/**
 * Destroys a client session. A null handle is a no-op.
 *
 * # Safety
 *
 * A non-null handle must have been returned by `cpxy_client_create`, and it must be destroyed at
 * most once. The handle must not be used after this call.
 */
void cpxy_client_destroy(cpxy_client_handle handle);

#ifdef __cplusplus
} // extern "C"
#endif // __cplusplus

#endif // CPXY_CLIENT_H
