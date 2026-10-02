# ملاحظات التعلم — المرحلة 3: WAL و Crash Recovery

## السؤال المركزي

> علاش قاعدة البيانات ما كتموتش ملي كيضرب التيار فوسط الكتابة؟

الجواب: **Write-Ahead Log** — قبل ما أي صفحة توصل للملف الأساسي، الصورة
الكاملة ديالها كتكتب فـ journal مع CRC32. و `commit()` = fsync ديال
الـ journal. هادشي هو خط الدفاع كامل.

## القاعدة الذهبية: WAL-first

```text
بدون WAL:  الكتابة → الملف (تالف ملي كيضرب التيار فوسطها)  💥
بـ WAL:    الكتابة → Journal → ... → checkpoint → الملف
                          ↑
              commit = fsync هان. من بعد هان، مضمونة 100%
```

النقطة الذكية فالتصميم ديالنا: **الملف الأساسي ما كيشوفش حتى صفحة
غير مكتملة المعاملة** — كيتكتبو فيه الصفحات غير فالـ checkpoint.
لهذا ما محتاجينش undo records: rollback = نسيان.

## الـ Recovery (شنو كيوقع ملي كتعاود فتح؟)

```text
اقرا الـ journal frame بـ frame
  └─ أول frame خاسر (CRC خايب / مقطوع / تسلسل ناقص) → وقفي، كولشي من بعدو = أنقاض الـ crash
  └─ خدي آخر commit frame → هادا هو الخط ديال الدبرabilité
  └─ replay: كل frame ملتزم، بالترتيب، كتكتب الصورة ديالو فالملف
  └─ reload meta + تفريغ الـ journal
```

**لاحظي**: الـ meta page (صفحة 0) كتتم journalيزي بحال أي صفحة. إلا
كانت "صفحة محررة (Free)" وصلت للملف قبل ما يكون الـ commit مسجل،
غادي نخسرو داتا ملتزمة! WAL-first كيمنع هادشي بالتصميم.

## هندسة الزواق: trait `PageIo`

```rust
pub trait PageIo {  // read/write/alloc/free/sync
}
impl PageIo for Pager      // خام: للأدوات والاختبارات
impl PageIo for Database   // بـ WAL: للمحرك الحقيقي
```

الـ B+Tree كيكتب مرة وحدة فوق `&mut dyn PageIo` — نفس الكود خدّام
فالجوج عوالم. هذا **polymorphism بلا vtable هدرة زايدة** — trait object
واحد كيغير سلوك النظام كامل.

## محاكاة الـ crash بلا signals

```rust
std::mem::forget(db);  // = kill -9: لا Drop، لا rollback، لا checkpoint
// من بعد: قطع الـ journal فـ 200 نقطة عشوائية والتحقق من الداتا
```

الاختبار الملكي: ملتزمة A فالملف، ملتزمة B فالـ journal فقط، قطع فأي
نقطة → **A كاملة ديما، B موجودة غير إلا نجت سلامة الـ commit frame**.
الـ invariant مطلق: ملتزمة = موجودة، غير ملتزمة = مغيبة.

## مفاهيم Rust

- **`BufWriter` + `get_ref()`**: الكتابة كتتراكم فالذاكرة، و fsync
  كيدير flush + sync_all على الملف الحقيقي.
- **Drop as a contract**: `impl Drop for Database` كيدير checkpoint
  عند الإغلاق النظيف — ولكن `mem::forget` كيتجاوزو تماماً (وهادا
  هو المطلوب: محاكاة موت مفاجئ).
- **trait objects**: `&mut dyn PageIo` — dispatch ديناميكي، الثمن
  مقبول لمستوى الـ I/O (كل عملية كتقيس الديسك أصلاً).

## الجاي: المرحلة 4 — SQL

الـ catalog ولّى جاهز (B+Tree داخل الـ meta). غادي نبنيو فوقو:
lexer → parser → planner → executor، و REPL كيهضر بـ SQL حقيقي،
و differential tests ضد SQLite.
