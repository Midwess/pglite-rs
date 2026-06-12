#include "pglite_native.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define BUF_CAP (1 << 20)

static unsigned char out_buf[BUF_CAP];
static size_t out_len, out_off;
static unsigned char in_buf[BUF_CAP];
static size_t in_len;

static ssize_t
read_cb(void *buffer, size_t max_length)
{
	size_t		n = out_len - out_off;

	if (n > max_length)
		n = max_length;
	memcpy(buffer, out_buf + out_off, n);
	out_off += n;
	return (ssize_t) n;
}

static ssize_t
write_cb(void *buffer, size_t length)
{
	memcpy(in_buf + in_len, buffer, length);
	in_len += length;
	return (ssize_t) length;
}

static void
put_i32be(unsigned char *p, uint32_t v)
{
	p[0] = (v >> 24) & 0xff;
	p[1] = (v >> 16) & 0xff;
	p[2] = (v >> 8) & 0xff;
	p[3] = v & 0xff;
}

static void
dump_messages(const char *label)
{
	size_t		off = 0;

	printf("%s:", label);
	while (off + 5 <= in_len)
	{
		unsigned char type = in_buf[off];
		uint32_t	len = (in_buf[off + 1] << 24) | (in_buf[off + 2] << 16) |
			(in_buf[off + 3] << 8) | in_buf[off + 4];

		printf(" %c(%u)", type, len);
		off += 1 + len;
	}
	printf("\n");
}

static int
has_message(unsigned char type)
{
	size_t		off = 0;

	while (off + 5 <= in_len)
	{
		uint32_t	len = (in_buf[off + 1] << 24) | (in_buf[off + 2] << 16) |
			(in_buf[off + 3] << 8) | in_buf[off + 4];

		if (in_buf[off] == type)
			return 1;
		off += 1 + len;
	}
	return 0;
}

int
main(int argc, char **argv)
{
	char		cmd[4096];
	char		bin[2048];
	const char *prefix;
	const char *pgdata;
	int			rc;

	if (argc != 3)
	{
		fprintf(stderr, "usage: smoke <install_prefix> <pgdata>\n");
		return 2;
	}
	prefix = argv[1];
	pgdata = argv[2];

	setenv("PGDATA", pgdata, 1);
	setenv("PGUSER", "postgres", 1);
	setenv("PGDATABASE", "postgres", 1);
	setenv("TZ", "UTC", 1);
	setenv("PGTZ", "UTC", 1);
	setenv("PGCLIENTENCODING", "UTF8", 1);
	setenv("LANG", "C", 1);

	snprintf(cmd, sizeof(cmd),
			 "'%s/bin/initdb' --allow-group-access --encoding UTF8 --locale=C "
			 "--locale-provider=libc --auth=trust -D '%s' > '%s.initdb.log' 2>&1",
			 prefix, pgdata, pgdata);
	rc = system(cmd);
	if (rc != 0)
	{
		fprintf(stderr, "FAIL: initdb rc=%d\n", rc);
		return 1;
	}
	printf("initdb: ok\n");

	pgl_native_setup();
	pgl_freopen("/dev/null", "r", 0);
	pgl_set_rw_cbs(read_cb, write_cb);
	pgl_setPGliteActive(1);

	snprintf(bin, sizeof(bin), "%s/bin/postgres", prefix);
	{
		char	   *pg_argv[] = {
			bin,
			"--single", "-F", "-O", "-j",
			"-c", "search_path=public",
			"-c", "exit_on_error=false",
			"-c", "log_checkpoints=false",
			"-c", "max_worker_processes=0",
			"-c", "max_parallel_workers=0",
			"-c", "max_parallel_workers_per_gather=0",
			"-c", "max_parallel_maintenance_workers=0",
			"-D", (char *) pgdata,
			"postgres",
			NULL
		};
		int			pg_argc = (int) (sizeof(pg_argv) / sizeof(pg_argv[0])) - 1;

		rc = pgl_native_call(pgl_backend_main, pg_argc, pg_argv);
	}
	if (rc != 99)
	{
		fprintf(stderr, "FAIL: backend main rc=%d (expected 99)\n", rc);
		return 1;
	}
	printf("backend main: ok (99)\n");

	pgl_startPGlite();

	{
		static const char params[] =
			"user\0postgres\0database\0postgres\0client_encoding\0UTF8\0";
		size_t		plen = sizeof(params);
		uint32_t	total = 4 + 4 + plen;

		out_off = 0;
		in_len = 0;
		put_i32be(out_buf, total);
		put_i32be(out_buf + 4, 196608);
		memcpy(out_buf + 8, params, plen);
		out_len = total;
	}

	rc = ProcessStartupPacket(pgl_getMyProcPort(), true, true);
	if (rc != 0)
	{
		fprintf(stderr, "FAIL: ProcessStartupPacket rc=%d\n", rc);
		return 1;
	}
	pgl_sendConnData();
	pgl_pq_flush();
	dump_messages("handshake");
	if (!has_message('R') || !has_message('Z'))
	{
		fprintf(stderr, "FAIL: handshake missing AuthenticationOk/ReadyForQuery\n");
		return 1;
	}
	printf("handshake: ok\n");

	{
		const char *sql = "SELECT 1;";
		uint32_t	total = 4 + strlen(sql) + 1;

		out_off = 0;
		out_len = 0;
		in_len = 0;
		out_buf[0] = 'Q';
		put_i32be(out_buf + 1, total);
		memcpy(out_buf + 5, sql, strlen(sql) + 1);
		out_len = 1 + total;
	}

	while (out_off < out_len || pq_buffer_remaining_data() > 0)
	{
		rc = pgl_native_pump();
		if (rc == 100)
			PostgresMainLongJmp();
	}
	PostgresSendReadyForQueryIfNecessary();
	pgl_pq_flush();
	dump_messages("select");

	if (!has_message('T') || !has_message('D') || !has_message('C') || !has_message('Z'))
	{
		fprintf(stderr, "FAIL: SELECT 1 missing T/D/C/Z messages\n");
		return 1;
	}

	printf("SMOKE PASS: SELECT 1 round-trip complete\n");
	return 0;
}
