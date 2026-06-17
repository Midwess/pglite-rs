use crate::db::PGlite;
use crate::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lsn(pub u64);

impl Lsn {
    pub fn from_pg_str(s: &str) -> Result<Lsn, Error> {
        let (hi, lo) = s.split_once('/').ok_or_else(|| Error::Lsn(s.to_string()))?;
        let hi = u64::from_str_radix(hi, 16).map_err(|_| Error::Lsn(s.to_string()))?;
        let lo = u64::from_str_radix(lo, 16).map_err(|_| Error::Lsn(s.to_string()))?;
        if hi > u32::MAX as u64 || lo > u32::MAX as u64 {
            return Err(Error::Lsn(s.to_string()));
        }
        Ok(Lsn((hi << 32) | lo))
    }

    pub fn to_pg_str(self) -> String {
        format!("{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

impl std::fmt::Display for Lsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_pg_str())
    }
}

pub(crate) struct ReplicaState {
    pub slot_name: String,
    pub publication: String,
    pub watermark: Lsn,
    pub fingerprint: String,
}

pub(crate) async fn ensure_meta_table(db: &PGlite) -> Result<(), Error> {
    db.exec(
        "CREATE TABLE IF NOT EXISTS _pglite_replica (
            id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
            slot_name text NOT NULL,
            publication text NOT NULL,
            watermark_lsn text NOT NULL,
            fingerprint text NOT NULL,
            security_version bigint NOT NULL DEFAULT 0,
            security_fingerprint text NOT NULL DEFAULT '',
            updated_at timestamptz NOT NULL DEFAULT now()
        )",
    )
    .await?;
    db.query(
        "ALTER TABLE _pglite_replica ADD COLUMN IF NOT EXISTS security_version bigint NOT NULL DEFAULT 0",
        &[],
    )
    .await?;
    db.query(
        "ALTER TABLE _pglite_replica ADD COLUMN IF NOT EXISTS security_fingerprint text NOT NULL DEFAULT ''",
        &[],
    )
    .await?;
    Ok(())
}

pub(crate) async fn load_state(db: &PGlite) -> Result<Option<ReplicaState>, Error> {
    let rows = db
        .query(
            "SELECT slot_name, publication, watermark_lsn, fingerprint FROM _pglite_replica WHERE id = 1",
            &[],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(ReplicaState {
        slot_name: row.get::<&str>(0)?.to_string(),
        publication: row.get::<&str>(1)?.to_string(),
        watermark: Lsn::from_pg_str(row.get::<&str>(2)?)?,
        fingerprint: row.get::<&str>(3)?.to_string(),
    }))
}

pub(crate) async fn init_state(
    db: &PGlite,
    slot_name: &str,
    publication: &str,
    watermark: Lsn,
    fingerprint: &str,
) -> Result<(), Error> {
    db.query(
        "INSERT INTO _pglite_replica (id, slot_name, publication, watermark_lsn, fingerprint)
         VALUES (1, $1, $2, $3, $4)
         ON CONFLICT (id) DO UPDATE SET
            slot_name = EXCLUDED.slot_name,
            publication = EXCLUDED.publication,
            watermark_lsn = EXCLUDED.watermark_lsn,
            fingerprint = EXCLUDED.fingerprint,
            updated_at = now()",
        &[
            &slot_name,
            &publication,
            &watermark.to_pg_str().as_str(),
            &fingerprint,
        ],
    )
    .await?;
    Ok(())
}

pub(crate) async fn update_fingerprint(db: &PGlite, fingerprint: &str) -> Result<(), Error> {
    db.query(
        "UPDATE _pglite_replica SET fingerprint = $1, updated_at = now() WHERE id = 1",
        &[&fingerprint],
    )
    .await?;
    Ok(())
}

pub(crate) async fn security_fingerprint(db: &PGlite) -> Result<Option<String>, Error> {
    let rows = db
        .query(
            "SELECT security_fingerprint FROM _pglite_replica WHERE id = 1",
            &[],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get::<&str>(0)?.to_string())),
        None => Ok(None),
    }
}

pub(crate) async fn bump_security(db: &PGlite, fingerprint: &str) -> Result<(), Error> {
    db.query(
        "UPDATE _pglite_replica SET security_version = security_version + 1, \
         security_fingerprint = $1, updated_at = now() WHERE id = 1",
        &[&fingerprint],
    )
    .await?;
    Ok(())
}

pub(crate) async fn security_version(db: &PGlite) -> Result<u64, Error> {
    let rows = db
        .query(
            "SELECT security_version::text FROM _pglite_replica WHERE id = 1",
            &[],
        )
        .await?;
    let value = rows
        .first()
        .map(|row| row.get::<&str>(0))
        .transpose()?
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::Lsn;

    #[test]
    fn lsn_round_trip() {
        for s in ["0/0", "16/B374D848", "FFFFFFFF/FFFFFFFF", "1/0"] {
            let lsn = Lsn::from_pg_str(s).unwrap();
            assert_eq!(lsn.to_pg_str(), s);
        }
    }

    #[test]
    fn lsn_value() {
        assert_eq!(Lsn::from_pg_str("0/1").unwrap(), Lsn(1));
        assert_eq!(Lsn::from_pg_str("1/0").unwrap(), Lsn(1 << 32));
        assert_eq!(
            Lsn::from_pg_str("16/B374D848").unwrap(),
            Lsn((0x16 << 32) | 0xB374D848)
        );
    }

    #[test]
    fn lsn_ordering() {
        assert!(Lsn::from_pg_str("0/FFFFFFFF").unwrap() < Lsn::from_pg_str("1/0").unwrap());
        assert!(Lsn(5) <= Lsn(5));
    }

    #[test]
    fn lsn_rejects_garbage() {
        for s in [
            "",
            "16",
            "16/",
            "/848",
            "xx/yy",
            "100000000/0",
            "16/B374D848/9",
        ] {
            assert!(Lsn::from_pg_str(s).is_err(), "{s} should fail");
        }
    }

    #[test]
    fn lsn_lowercase_accepted() {
        assert_eq!(
            Lsn::from_pg_str("16/b374d848").unwrap(),
            Lsn::from_pg_str("16/B374D848").unwrap()
        );
    }
}
