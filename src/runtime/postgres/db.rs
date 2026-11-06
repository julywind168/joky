//! All futures here run on the database service thread.
use super::*;
use futures_util::TryStreamExt;
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{Arguments, AssertSqlSafe, Column, Postgres, Row, TypeInfo, ValueRef};

// PostgreSQL's built-in OIDs and binary payloads. Keep JSON numbers as text,
// without routing them through a host floating-point representation.
struct WireValue<const OID: u32>(Vec<u8>);
impl<const OID: u32> sqlx::Type<Postgres> for WireValue<OID> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_oid(sqlx::postgres::types::Oid(OID))
    }
}
impl<const OID: u32> sqlx::Encode<'_, Postgres> for WireValue<OID> {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        buf.extend_from_slice(&self.0);
        Ok(sqlx::encode::IsNull::No)
    }
}
const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;
const TIMESTAMP_MICROS: std::ops::RangeInclusive<i64> =
    -62_135_596_800_000_000..=253_402_300_799_999_999;

// An incomplete exchange can never be recycled. Only a fully consumed reply
// takes the connection out of this guard for normal pool return.
pub(super) struct Loan(Option<sqlx::pool::PoolConnection<Postgres>>);
impl Drop for Loan {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            drop(conn.detach());
        }
    }
}
impl Loan {
    fn conn(&mut self) -> &mut sqlx::PgConnection {
        self.0.as_mut().unwrap()
    }
    fn recycle(mut self) {
        drop(self.0.take());
    }
}
pub(super) enum Target {
    Pool(sqlx::PgPool),
    Tx(Loan),
}
impl Target {
    async fn checkout(self) -> Result<Loan, Fault> {
        match self {
            Self::Tx(loan) => Ok(loan),
            Self::Pool(pool) => pool
                .acquire()
                .await
                .map(|c| Loan(Some(c)))
                .map_err(|error| match error {
                    sqlx::Error::PoolTimedOut => Fault::AcquireTimeout,
                    _ => Fault::Acquire,
                }),
        }
    }
}
#[derive(Debug)]
pub(super) enum Fault {
    Invalid,
    Unavailable,
    Closed,
    Limit,
    Unknown,
    Database(String),
    ResultType,
    ResultLimit,
    Connect(String),
    Acquire,
    AcquireTimeout,
    MultipleRows,
    ResultRange,
}
impl Fault {
    pub(super) fn code(&self) -> u64 {
        match self {
            Self::Invalid => 1,
            Self::Unavailable => 2,
            Self::Closed => 3,
            Self::Limit => 4,
            Self::Unknown => 5,
            Self::Database(_) => 6,
            Self::ResultType => 7,
            Self::ResultLimit => 8,
            Self::Connect(_) => 9,
            Self::Acquire => 10,
            Self::AcquireTimeout => 11,
            Self::MultipleRows => 12,
            Self::ResultRange => 13,
        }
    }
}
fn failure(e: sqlx::Error, uncertain: bool) -> Fault {
    if let sqlx::Error::Database(db) = e {
        let code = db.code().unwrap_or_default().into_owned();
        if uncertain && (code.starts_with("08") || code == "40003" || code == "57P01") {
            return Fault::Unknown;
        }
        Fault::Database(code)
    } else if uncertain {
        Fault::Unknown
    } else {
        Fault::Unavailable
    }
}
pub(super) async fn open(url: String, max: u32) -> Result<Answer, Fault> {
    let opts: PgConnectOptions = url.parse().map_err(|_| Fault::Invalid)?;
    // Downgrade is allowed only when the trusted host explicitly selects disable.
    let plaintext = url
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|p| p == "sslmode=disable"));
    if plaintext && !matches!(opts.get_ssl_mode(), PgSslMode::Disable) {
        return Err(Fault::Invalid);
    }
    let opts = opts
        .ssl_mode(if plaintext {
            PgSslMode::Disable
        } else {
            PgSslMode::VerifyFull
        })
        .statement_cache_capacity(0);
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .after_release(|conn, _| {
            Box::pin(async move {
                // Reset session settings, temp tables and advisory locks. A stray open
                // transaction makes DISCARD fail, so SQLx evicts that connection.
                sqlx::raw_sql("DISCARD ALL").execute(conn).await?;
                Ok(true)
            })
        })
        .connect_with(opts)
        .await
        .map_err(|e| {
            Fault::Connect(
                e.as_database_error()
                    .and_then(|db| db.code())
                    .unwrap_or_default()
                    .into_owned(),
            )
        })?;
    Ok(Answer::new(Value::Pool(pool)))
}
pub(super) async fn begin(target: Target) -> Result<Answer, Fault> {
    let mut loan = target.checkout().await?;
    sqlx::query("BEGIN")
        .execute(loan.conn())
        .await
        .map_err(|e| failure(e, false))?;
    Ok(Answer::new(Value::Tx(Mutex::new(Some(loan)))))
}
pub(super) async fn finish(mut loan: Loan, commit: bool) -> Result<Answer, Fault> {
    sqlx::query(if commit { "COMMIT" } else { "ROLLBACK" })
        .execute(loan.conn())
        .await
        .map_err(|e| failure(e, commit))?;
    loan.recycle();
    Ok(Answer::new(Value::Count(0)))
}
fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], Fault> {
    let (a, b) = input.split_at_checked(n).ok_or(Fault::Invalid)?;
    *input = b;
    Ok(a)
}
fn arguments(mut input: &[u8]) -> Result<PgArguments, Fault> {
    let mut args = PgArguments::default();
    while !input.is_empty() {
        if args.len() >= 256 {
            return Err(Fault::Limit);
        }
        let tag = take(&mut input, 1)?[0];
        match tag {
            0 => args.add(Option::<bool>::None),
            1 => args.add(Option::<i64>::None),
            2 => args.add(Option::<String>::None),
            3 => args.add(Option::<Vec<u8>>::None),
            8 => args.add(Option::<WireValue<1184>>::None),
            9 => args.add(Option::<WireValue<2950>>::None),
            10 => args.add(Option::<WireValue<3802>>::None),
            11 => {
                let micros = i64::from_le_bytes(take(&mut input, 8)?.try_into().unwrap());
                if !TIMESTAMP_MICROS.contains(&micros) {
                    return Err(Fault::Invalid);
                }
                args.add(WireValue::<1184>(
                    (micros - PG_EPOCH_MICROS).to_be_bytes().to_vec(),
                ))
            }
            4 => args.add(match take(&mut input, 1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(Fault::Invalid),
            }),
            5 => args.add(i64::from_le_bytes(take(&mut input, 8)?.try_into().unwrap())),
            6 | 7 | 12 | 13 => {
                let n = u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap());
                let bytes = take(&mut input, n as usize)?;
                if tag == 6 {
                    args.add(std::str::from_utf8(bytes).map_err(|_| Fault::Invalid)?)
                } else if tag == 7 {
                    args.add(bytes)
                } else if tag == 12 {
                    if bytes.len() != 16 {
                        return Err(Fault::Invalid);
                    }
                    args.add(WireValue::<2950>(bytes.to_vec()))
                } else {
                    std::str::from_utf8(bytes).map_err(|_| Fault::Invalid)?;
                    let mut json = Vec::with_capacity(bytes.len() + 1);
                    json.push(1);
                    json.extend_from_slice(bytes);
                    args.add(WireValue::<3802>(json))
                }
            }
            _ => return Err(Fault::Invalid),
        }
        .map_err(|_| Fault::Invalid)?;
    }
    Ok(args)
}
fn row(out: &mut Vec<u8>, row: sqlx::postgres::PgRow, named: bool) -> Result<(), Fault> {
    if row.len() > 128 || out.len() + 4 > MAX {
        return Err(Fault::ResultLimit);
    }
    out.extend((row.len() as u32).to_le_bytes());
    for (i, col) in row.columns().iter().enumerate() {
        let tag = match col.type_info().name() {
            "BOOL" => 0,
            "INT2" | "INT4" | "INT8" => 1,
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => 2,
            "BYTEA" => 3,
            "TIMESTAMPTZ" => 8,
            "UUID" => 9,
            "JSONB" => 10,
            _ => return Err(Fault::ResultType),
        };
        if !named && tag >= 8 {
            return Err(Fault::ResultType);
        }
        if named {
            let name = col.name().as_bytes();
            if 4 + name.len() > MAX - out.len() {
                return Err(Fault::ResultLimit);
            }
            out.extend((name.len() as u32).to_le_bytes());
            out.extend(name);
        }
        let raw = row.try_get_raw(i).map_err(|_| Fault::ResultType)?;
        if out.len() + 1 > MAX {
            return Err(Fault::ResultLimit);
        }
        if raw.is_null() {
            out.push(tag);
            continue;
        }
        out.push(if tag < 4 { tag + 4 } else { tag + 3 });
        let bytes = raw.as_bytes().map_err(|_| Fault::ResultType)?;
        let bytes = if tag == 10 {
            if bytes.first() != Some(&1) {
                return Err(Fault::ResultType);
            }
            &bytes[1..]
        } else {
            bytes
        };
        let payload = match tag {
            0 => 1,
            1 | 8 => 8,
            _ => 4 + bytes.len(),
        };
        if payload > MAX - out.len() {
            return Err(Fault::ResultLimit);
        }
        match tag {
            0 => out.push(u8::from(
                row.try_get::<bool, _>(i).map_err(|_| Fault::ResultType)?,
            )),
            1 => {
                let n = match bytes.len() {
                    2 => i16::from_be_bytes(bytes.try_into().unwrap()) as i64,
                    4 => i32::from_be_bytes(bytes.try_into().unwrap()) as i64,
                    8 => i64::from_be_bytes(bytes.try_into().unwrap()),
                    _ => return Err(Fault::ResultType),
                };
                out.extend(n.to_le_bytes());
            }
            8 => {
                let micros = i64::from_be_bytes(bytes.try_into().map_err(|_| Fault::ResultType)?)
                    .checked_add(PG_EPOCH_MICROS)
                    .filter(|value| TIMESTAMP_MICROS.contains(value))
                    .ok_or(Fault::ResultRange)?;
                out.extend(micros.to_le_bytes());
            }
            _ => {
                if tag == 9 && bytes.len() != 16 {
                    return Err(Fault::ResultType);
                }
                if tag == 2 || tag == 10 {
                    std::str::from_utf8(bytes).map_err(|_| Fault::ResultType)?;
                }
                out.extend((bytes.len() as u32).to_le_bytes());
                out.extend(bytes);
            }
        }
    }
    Ok(())
}
pub(super) enum Mode {
    Execute,
    Query { optional: bool, named: bool },
}
pub(super) async fn run(
    target: Target,
    sql: String,
    params: Vec<u8>,
    mode: Mode,
) -> Result<Answer, Fault> {
    let tx = matches!(target, Target::Tx(_));
    let args = arguments(&params)?;
    let mut loan = target.checkout().await?;
    let request = sqlx::query_with(AssertSqlSafe(sql), args).persistent(false);
    let value = if let Mode::Query { optional, named } = mode {
        let mut rows = request.fetch(loan.conn());
        let mut out = Vec::new();
        let mut count = 0;
        while let Some(r) = rows.try_next().await.map_err(|e| failure(e, !tx))? {
            count += 1;
            if count > 1024 {
                return Err(Fault::ResultLimit);
            }
            // Consume the reply before returning this connection to the pool
            // or transaction; only the first row needs a snapshot in Optional mode.
            if count == 1 || !optional {
                row(&mut out, r, named)?;
            }
        }
        if count > 1 && optional {
            Value::Fault(Fault::MultipleRows)
        } else {
            Value::Bytes(out)
        }
    } else {
        Value::Count(
            request
                .execute(loan.conn())
                .await
                .map_err(|e| failure(e, !tx))?
                .rows_affected(),
        )
    };
    let restore = if tx {
        Some(loan)
    } else {
        loan.recycle();
        None
    };
    Ok(Answer { value, restore })
}
