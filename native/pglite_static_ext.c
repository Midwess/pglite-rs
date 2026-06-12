typedef int pglite_static_ext_no_empty_tu;

#ifdef _WIN32

extern void pgl_register_static_ext(const char *name,
									const char *const *symbol_names,
									void *const *symbol_addrs, int nsyms);

extern void Pg_magic_func(void);
extern void _PG_init(void);
extern void plpgsql_call_handler(void);
extern void plpgsql_inline_handler(void);
extern void plpgsql_validator(void);
extern void pg_finfo_plpgsql_call_handler(void);
extern void pg_finfo_plpgsql_inline_handler(void);
extern void pg_finfo_plpgsql_validator(void);

static const char *const plpgsql_symbol_names[] = {
	"Pg_magic_func",
	"_PG_init",
	"plpgsql_call_handler",
	"plpgsql_inline_handler",
	"plpgsql_validator",
	"pg_finfo_plpgsql_call_handler",
	"pg_finfo_plpgsql_inline_handler",
	"pg_finfo_plpgsql_validator",
};

static void *const plpgsql_symbol_addrs[] = {
	(void *) Pg_magic_func,
	(void *) _PG_init,
	(void *) plpgsql_call_handler,
	(void *) plpgsql_inline_handler,
	(void *) plpgsql_validator,
	(void *) pg_finfo_plpgsql_call_handler,
	(void *) pg_finfo_plpgsql_inline_handler,
	(void *) pg_finfo_plpgsql_validator,
};

__attribute__((constructor)) static void
pgl_register_plpgsql(void)
{
	pgl_register_static_ext("plpgsql", plpgsql_symbol_names,
							plpgsql_symbol_addrs, 8);
}

#endif
