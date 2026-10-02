# ملاحظات التعلم — المرحلة 5: MVCC والفهارس الثانوية

## الفكرة المركزية: كل صف هي "نسخة"

```text
[xmin u64][xmax u64][الصف]
   ↑         ↑
  من خلقه   من حذفه (0 = مازال عايش)
```

- **UPDATE = نسخة جديدة**: النسخة القديمة كتعلّم بأنها محذوفة (tombstone)،
  والجديدة كتكتب فـ row id جديد. القديمة تبقى فالشجرة (GC لاحقاً).
- **القراءة كتفلتر بالـ visibility**: الصف كيبان ليك غير إلا خلقته
  نتا، ولا خالقو ملتزم **قبل** ما تبدا transaction ديالك. حذف
  وقع **بعد** ما تبدا → مازال كتبان ليك. هادا هو snapshot isolation.

## القاعدة اللي كتجمع كلشي: "مجهول = ملتزم"

Unknown transaction id ⇒ Committed. علاش؟ حيت الـ crash كينقي أي
journal record غير ملتزم — فأي شيء بقا فالديسك يعني ملتزم. بهاد
القاعدة الواحدة، MVCC والـ WAL كيتكاملو بلا أي glue.

## الهندسة: sessions فوق Database واحد

```rust
#[derive(Clone)]
pub struct SqlEngine { db: Arc<Mutex<Database>>, txn: Option<TxnContext> }
```

كل clone = session مستقلة. **قاعدة الكاتب الوحيد** (بحال SQLite
الافتراضي): transaction تخزينية واحدة فنفس الوقت، والكاتب الثاني كيتلقى
`Locked`. القراءات ما كتحتاجش storage transaction — الـ visibility
هو اللي كيفلتر.

## الفهارس الثانوية: فن الـ byte encoding

- المفتاح = `[قيمة العمود][row id BE]` — الـ row id كيخلي المفاتيح
  فريدين ويشير للصف.
- **Integers بترتيب عددي**: BE مباشر كيعطي ترتيب خاطئ للسوالب
  (-5 > 3 بالبايت!) — الحل: XOR مع `i64::MIN` قبل التشفير.
- **حدود الـ prefix scan**: `=` كيمشي من `value+rid=0` حتى
  `prefix_upper_bound(value)` — هاد الأخير كيزيد آخر بايت بالحمل
  (carry)، وكل-0xFF يعني بلا حدود. خوارزمية 10 أسطر كتفصل الصف
  من الغلط.
- **NULL ما كيتفهرسش** — IS NULL كيدير full scan. انحراف موثق عن
  SQLite.

## المبدأ الذهبي: الـ planner ماشي مصدر الحقيقة

الـ index scan كيجيب الصفوف، ومن بعد **الـ filter الكامل كيتعاد**.
يعني: إلا كان الـ planner غالط، النتيجة تبقى صحيحة (أبطأ فقط).
الصحة ما تعتمدش أبداً على ذكاء الـ planner.

## Rust تعلمناها

- **Arc<Mutex<T>>**: مشاركة الحالة بين sessions — `Clone` رخيص.
- **Borrow checker كيفرض architecture**: كان الـ scan كيحتفظ بـ `&mut db`
  واللووب كتقرا من `db` → الحل: `collect()` الـ iterator قبل المعالجة
  (materialization). التقييد ديال Rust صنعنا حل معمارياً واضح.
- **Errors كأنواع**: `DbError::NestedTransaction` ماشي string matching —
  الـ compiler كيفرض عليك تعالج كل حالة.

## الجاي: المرحلة 6 — psql wire protocol
