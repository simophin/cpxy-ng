/* The C ABI of the mobile-engine packet engine in gateway mode, for cpxy-tsgw (gateway/).
 * Kept by hand in step with mobile-engine/src/capi.rs, which documents each function in full.
 *
 * Thread safety: call from any thread. cpxy_engine_dns_resolve, cpxy_engine_is_running and
 * cpxy_engine_traffic may run concurrently with each other; cpxy_engine_stop and
 * cpxy_engine_free must not run concurrently with any other call on the same handle, and a
 * handle must not be used after cpxy_engine_free. */

#ifndef CPXY_ENGINE_H
#define CPXY_ENGINE_H

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* The query is malformed, an argument is invalid, or resolving failed. */
#define CPXY_ENGINE_ERR_FAILED -1
/* The engine is stopped. */
#define CPXY_ENGINE_ERR_STOPPED -2
/* The response does not fit the output buffer. */
#define CPXY_ENGINE_ERR_TOO_SMALL -3

typedef struct CpxyEngine CpxyEngine;

#ifdef __cplusplus
extern "C" {
#endif

/* Starts the engine in gateway mode on fd, which moves one IP packet per read and write (a
 * SOCK_SEQPACKET socket). config_json is the apps' engine config. Blocks while the DNS servers are
 * set up. Takes ownership of a non-negative fd even on failure. Returns NULL on failure, with the
 * reason in error_buffer when it is non-NULL. */
CpxyEngine *cpxy_engine_start(int fd,
                              const char *config_json,
                              char *error_buffer,
                              uint32_t error_buffer_capacity);

/* Stops the engine and closes fd; blocks for up to a few seconds. Repeated calls do nothing. */
void cpxy_engine_stop(const CpxyEngine *engine);

/* Stops the engine if it runs and frees the handle. NULL does nothing. */
void cpxy_engine_free(CpxyEngine *engine);

/* False once the packet worker has ended. */
bool cpxy_engine_is_running(const CpxyEngine *engine);

/* IP bytes into the engine (sent) and back (received). Either pointer may be NULL. */
void cpxy_engine_traffic(const CpxyEngine *engine, uint64_t *sent, uint64_t *received);

/* Answers one wire-format DNS query with the engine's split DNS, writing the response to out.
 * Blocks until answered. Returns the response length or a negative CPXY_ENGINE_ERR_* code.
 * 65535 bytes of out always suffice. */
int64_t cpxy_engine_dns_resolve(const CpxyEngine *engine,
                                const uint8_t *query,
                                size_t query_len,
                                uint8_t *out,
                                size_t out_capacity);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* CPXY_ENGINE_H */
