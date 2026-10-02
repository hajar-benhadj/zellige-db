# ملاحظات التعلم — المرحلة 6: Postgres wire protocol

## الهدف

`psql -h 127.0.0.1 -p 5432` يتصل بقاعدة البيانات **اللي بنيناها من الصفر**
وما يعرفش أنها ماشي PostgreSQL. هادي أقوى دقيقة فأي ديمو.

## البروتوكول: bytes بحال بحال

```text
Client                               Server
  │── SSLRequest ──────────────────────►│ 'N' (لا TLS)
  │── Startup (protocol 3.0) ──────────►│
  │◄─────────────── 'R' AuthenticationOk│
  │◄─────────────── 'S' ParameterStatus │
  │◄─────────────── 'K' BackendKeyData  │
  │◄─────────────── 'Z' ReadyForQuery ──│  ('I' = idle)
  │── 'Q' "SELECT ..." ────────────────►│
  │◄──── 'T' RowDescription ────────────│
  │◄──── 'D' DataRow × n ───────────────│
  │◄──── 'C' "SELECT 2" ────────────────│
  │◄──── 'Z' ───────────────────────────│
```

**تفاصيل كتفرق:**
- **Big-endian** (network order) — ماشي بحال صفحاتنا little-endian!
- الـ length ديال الرسالة كيعد نفسو، ماشي الـ type byte.
- NULL فالـ DataRow = طول `-1` ماشي نص فارغ.
- `t`/`f` للـ BOOLEAN (عادة Postgres، ماشي true/false).
- `Z` بعد كل `Q` وبها حالة الـ transaction: `I` ولا `T`.

## SQLSTATE codes حقيقية

الأخطاء كتوصل بـ codes ديال Postgres الحقيقيين: `42P01` (undefined_table)،
`42601` (syntax)، `55P03` (lock_not_available). الـ psql كي display
`ERROR: database is locked by another transaction (55P03)` — كأنو
PostgreSQL حقيقي.

## الاختبار: عميل مصغر بيدنا (150 سطر)

بلا مكتبات: `TcpStream` + handshake + `Q`/`Z` loop. كيختبر **على
مستوى البايت** — نفس المستوى اللي البروتوكول عايش فيه:
- SSL decline byte بالضبط `N`
- حالة الـ transaction فالـ `Z` (`I`/`T`)
- session ثانية: error + ما كاينش connection poisoning

## refactoring كتعلمنا منه

- **`execute_batch` + streaming callback**: الـ wire كي sends statements
  بالباكاتش، وكل output كيتوصل للعميل مباشرة — ماشي buffering.
- **`parse_all`**: parser واحد، استعمالين: `execute` (statement واحد)
  و `execute_batch` (باكاتش).
- **Binary + library crate**: `zdb-server` ولّى عندها lib (`zdb_server`)
  باش الـ tests تستعمل `serve_on` على port عشوائي.

## المتبقي (v0.2)

Extended query mode (`Parse`/`Bind`/`Execute`) — بعض GUI clients
كيطلبوها. TLS. Auth حقيقي.

## الجاي: المرحلة 7 — Benchmarks بالأرقام
