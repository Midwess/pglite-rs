#include "pglite_native.h"

#include <fcntl.h>
#include <stdlib.h>
#include <setjmp.h>
#include <unistd.h>

#ifdef _WIN32
#define sigjmp_buf jmp_buf
#define sigsetjmp(env, savesigs) setjmp(env)
#endif

#define PGL_TRAMP_MAX 8
extern sigjmp_buf pgl_tramp[PGL_TRAMP_MAX];
extern int	pgl_tramp_top;

extern int	postmaster_alive_fds[2];
extern bool IsUnderPostmaster;
extern bool IsPostmasterEnvironment;
extern int	whereToSendOutput;
extern void *MyProcPort;
extern void *UsedShmemSegAddr;
extern void pgl_shmem_reset(void);
extern void pgl_fd_reset(void);
extern void pgl_xlog_fd_reset(void);

void
pgl_native_setup(void)
{
#ifndef _WIN32
	if (postmaster_alive_fds[0] == -1)
	{
		pipe(postmaster_alive_fds);
		fcntl(postmaster_alive_fds[0], F_SETFL, O_NONBLOCK);
	}
#endif
}

void
pgl_native_reset(void)
{
	IsUnderPostmaster = false;
	IsPostmasterEnvironment = false;
	whereToSendOutput = 1;
	MyProcPort = NULL;
	UsedShmemSegAddr = NULL;
	pgl_shmem_reset();
	pgl_fd_reset();
	pgl_xlog_fd_reset();
}

int
pgl_native_pump(void)
{
	int			jumped;

	pgl_tramp_top++;
	jumped = sigsetjmp(pgl_tramp[pgl_tramp_top], 1);
	if (jumped != 0)
	{
		pgl_tramp_top--;
		return jumped - 1;
	}
	PostgresMainLoopOnce();
	pgl_tramp_top--;
	return -1;
}
