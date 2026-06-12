#include "pglite_native.h"

#include <setjmp.h>
#include <stdlib.h>

#ifdef _WIN32
#include <winsock2.h>
#define sigjmp_buf jmp_buf
#define sigsetjmp(env, savesigs) setjmp(env)
#define siglongjmp longjmp
#else
#include <poll.h>
#endif

int
pgl_native_poll(void *fds, unsigned long nfds, int timeout)
{
#ifdef _WIN32
	return WSAPoll((WSAPOLLFD *) fds, (ULONG) nfds, timeout);
#else
	return poll((struct pollfd *) fds, (nfds_t) nfds, timeout);
#endif
}

int
pgl_native_setitimer(int which, const void *new_value, void *old_value)
{
	(void) which;
	(void) new_value;
	(void) old_value;
	return 0;
}

#define PGL_TRAMP_MAX 8

sigjmp_buf	pgl_tramp[PGL_TRAMP_MAX];
int			pgl_tramp_top = -1;

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
