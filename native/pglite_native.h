#ifndef PGLITE_NATIVE_H
#define PGLITE_NATIVE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <sys/types.h>

struct Port;

typedef ssize_t (*pgl_read_t)(void *buffer, size_t max_length);
typedef ssize_t (*pgl_write_t)(void *buffer, size_t length);
typedef ssize_t (*pglite_system_t)(const char *command);
typedef FILE *(*pglite_popen_t)(const char *command, const char *mode);
typedef int (*pglite_pclose_t)(FILE *stream);

void pgl_set_rw_cbs(pgl_read_t read_cb, pgl_write_t write_cb);
void pgl_set_system_fn(pglite_system_t system_fn);
void pgl_set_popen_fn(pglite_popen_t popen_fn);
void pgl_set_pclose_fn(pglite_pclose_t pclose_fn);

FILE *pgl_freopen(const char *pathname, const char *mode, int streamid);

int pgl_setPGliteActive(int newValue);
void pgl_startPGlite(void);
void pgl_run_atexit_funcs(void);
void pgl_shmem_reset(void);
void pgl_native_reset(void);
void clear_setitimer(void);

int pgl_initdb_main(int argc, char **argv);
int pgl_backend_main(int argc, char **argv);

struct Port *pgl_getMyProcPort(void);
int ProcessStartupPacket(struct Port *port, bool ssl_done, bool gss_done);
void pgl_sendConnData(void);

void PostgresMainLoopOnce(void);
void PostgresMainLongJmp(void);
void PostgresSendReadyForQueryIfNecessary(void);
ssize_t pq_buffer_remaining_data(void);
void pgl_pq_flush(void);
bool IsTransactionBlock(void);

void pgl_native_setup(void);
void pgl_native_exit(int status);
int pgl_native_call(int (*entry)(int, char **), int argc, char **argv);
int pgl_native_pump(void);

#endif
