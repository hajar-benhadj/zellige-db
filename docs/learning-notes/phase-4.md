# ملاحظات التعلم — المرحلة 4: SQL من الصفر

## شنو درنا؟

بنينا SQL front-end كامل بلا حتى مكتبة parser: **lexer → recursive-descent
parser → AST → executor** بأسلوب Volcano، فوق محرك الـ WAL.

## البنية: كل table هي B+Tree

```text
zdb:tables (شجرة الـ schemas)
  └─ "users" → [schema: أعمدة + أنواع + rowid counter]

zdb:t:users (شجرة الداتا)
  └─ row_id (u64 BE) → [null bitmap][قيم مشفرة]
```

- **u64 BE كـ key** = الترتيب فالشجرة هو ترتيب الإدخال (clustered index
  بأسلوب InnoDB).
- **null bitmap** فمقدمة السجل: bit مولود = العمود NULL — ما كنخزنوش
  أصلاً.
- الجدول الجديد كيرث كل شي: schema كيتخزن فشجرة، والشجرة فالـ catalog،
  والـ catalog فالـ meta page — **DDL كيكون crash-safe بالمجان**.

## Parser: recursive descent بسيط ومنظم

الأولوية (من الأضعف للأقوى):

```text
OR  <  AND  <  NOT  <  المقارنات  <  + -  <  * / %  <  unary  <  primary
```

كل مستوى دالة: `or_expr()` كتعيط لـ `and_expr()` اللي كتعيط
لـ `not_expr()`... هادا هو recursive descent كامل.

## الاختبار التفاضلي ضد SQLite (الفكرة الأهم)

> حدا خاصو يحكم على المحرك ديالك — ماشي نتا.

```text
نفس SQL عشوائي  →  ZelligeDB  ─┐
                               ├─ النتائج خاصهم يكونو متطابقين بالضبط
نفس SQL عشوائي  →  SQLite    ─┘
```

- 300 سجل عشوائي + 150 استعلام عشوائي (WHERE / LIKE / ORDER BY / LIMIT)
- `id` دائماً فالـ ORDER BY كـ tiebreaker باش الترتيب يكون deterministe
  فالجوج محركات
- `diff-sqlite` feature: rusqlite (C library) كيتجمع فقط فالـ CI Linux —
  لأن toolchain ديالنا GNU ما فيهش C compiler كامل، والفرضية هي
  "الـ CI هو اللي كيثبت الصحة"

## Bug حقيقي صطدناه فهاد المرحلة

**ORDER BY بعد الـ projection**: كنا كنرتبو الصفوف بعد ما اقتطعنا
الأعمدة — فرتیجال: ترتيب على عمود ماشي مطلوب → index out of bounds!
التصحيح: **sort على الصفوف الكاملة، ثم project**. الاختبار
`null_semantics` هو لي كتشافو.

## SQL semantics اختصرناها بوعي (وموثقينها فالـ ADR)

- المقارنة مع NULL → false (ماشي three-valued logic — IS NULL هو الحل)
- NULLs فالأول فالترتيب التصاعدي (بحال SQLite)
- types صارمة: `score > 'abc'` = خطأ، ماشي coercion

## الجاي: المرحلة 5

فهارس ثانوية (planner كيختار index scan) + **MVCC** بـ snapshot
isolation مع tests كتثبت غياب anomalies.
