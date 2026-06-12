#include "pglite_native.h"

#include <setjmp.h>
#include <stdlib.h>

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
