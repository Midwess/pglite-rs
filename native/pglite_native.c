#include "pglite_native.h"

#include <fcntl.h>
#include <setjmp.h>
#include <stdlib.h>
#include <unistd.h>

extern int	postmaster_alive_fds[2];

void
pgl_native_setup(void)
{
	if (postmaster_alive_fds[0] == -1)
	{
		pipe(postmaster_alive_fds);
		fcntl(postmaster_alive_fds[0], F_SETFL, O_NONBLOCK);
	}
}

int
pgl_native_setitimer(int which, const void *new_value, void *old_value)
{
	(void) which;
	(void) new_value;
	(void) old_value;
	return 0;
}

extern bool IsUnderPostmaster;
extern bool IsPostmasterEnvironment;
extern int	whereToSendOutput;
extern void *MyProcPort;
extern void *UsedShmemSegAddr;
extern void pgl_shmem_reset(void);
extern void pgl_fd_reset(void);
extern void pgl_xlog_fd_reset(void);

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

#define PGL_TRAMP_MAX 8

static sigjmp_buf pgl_tramp[PGL_TRAMP_MAX];
static int pgl_tramp_top = -1;

void
pgl_native_exit(int status)
{
	if (pgl_tramp_top >= 0)
		siglongjmp(pgl_tramp[pgl_tramp_top], status + 1);
	exit(status);
}

int
pgl_native_call(int (*entry)(int, char **), int argc, char **argv)
{
	int			rc;
	int			jumped;

	pgl_tramp_top++;
	jumped = sigsetjmp(pgl_tramp[pgl_tramp_top], 1);
	if (jumped != 0)
	{
		pgl_tramp_top--;
		return jumped - 1;
	}
	rc = entry(argc, argv);
	pgl_tramp_top--;
	return rc;
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
