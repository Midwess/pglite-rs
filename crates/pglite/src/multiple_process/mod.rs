use crate::db::LocaleProvider;

#[derive(Clone, Debug)]
pub struct MultiProcessOptions {
    pub username: String,
    pub database: String,
    pub max_connections: usize,
    pub relaxed_durability: bool,
    pub start_params: Vec<String>,
    pub locale_provider: LocaleProvider,
}

impl Default for MultiProcessOptions {
    fn default() -> MultiProcessOptions {
        MultiProcessOptions {
            username: "postgres".into(),
            database: "postgres".into(),
            max_connections: 4,
            relaxed_durability: false,
            start_params: Vec::new(),
            locale_provider: LocaleProvider::default(),
        }
    }
}
