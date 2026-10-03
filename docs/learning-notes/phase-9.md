# ملاحظات التعلم — المرحلة 9: الإطلاق

## شنو كيبقى بعد ما يخدم كولشي؟

المرحلة الأخيرة ماشي تقنية بقدر ما هي **انضباط**:

1. **CHANGELOG**: كل ميزة كتبلى غير فالـ git log — الإنسان اللي كيزور
   الـ repo خاصو يشوف القصة فملف واحد. Format: Keep a Changelog.
2. **Blog post**: الكود ما كيهضرش بوحدو. المقال "I built a database
   engine from scratch" كيحول شهور ديال الخدمة لأصل مهني: هوشة ديال
   الحوكي، قصة الـ cache bug، الأرقام.
3. **الـ tag على commit أخضر**: أول tag تعملنا كان على commit
   والـ CI فيه أحمر (bug ديال String/&str فالـ differential test،
   من بعد normalization ديال BOOLEAN). الحل: `git tag -f` + إعادة
   الـ release. **القاعدة: release غير على CI أخضر.**
4. **الأخطاء الأخيرة كانو فالاختبارات نفسها** ماشي فالمحرك —
   الـ differential suite (اللي كيتجمع غير فالـ CI Linux) لقا:
   - mismatch ديال الأنواع (String/&str)
   - اختلاف الـ rendering: BOOLEAN عندنا `true/false` و SQLite
     `1/0` — **الداتا كلها متطابقة** من أول تشغيل حقيقي

## الحصيلة النهائية ديال المشروع كامل

| الطبقة | الملفات | الاختبارات |
|---|---|---|
| zdb-core (pages, btree, wal, database) | ~2500 سطر | 31 |
| zdb-sql (lexer, parser, exec, mvcc) | ~1900 سطر | 32 |
| zdb-server (wire, REPL) | ~600 سطر | 2 |
| الاختبارات التكاملية + property + differential + crash | — | 65 المجموع |

9 ADRs · 9 ملفات ديال ملاحظات التعلم · CI فـ 3 أنظمة · differential
فالـ Linux · playground على GitHub Pages.

## الدروس الكبيرة ديال المشروع كامل

1. **Correctness قبل الأداء**: crash tests قبل benchmarks — وكاش
   عادلة: الـ crash suite صطاد الـ cache bug اللي سمح الكاش نفسو.
2. **الاختبارات العشوائية + seed مطبوع** = reproducibility.
3. **المقارنة مع مرجع مستقل** (SQLite، BTreeMap) هي الفرق بين
   "كيخدم" و"صحيح".
4. **ADR كيحميك من نسيان علاش اختاريتي** — 9 مرات رجعت ليهم.
5. **الحدود الصارمة كيتحولو لميزات**: deny(unsafe_code)، بلا
   parser library، بلا حتى dependency تقيلة.

## فين كملنا — وفين غادي نكملو (v0.2)

Extended query mode · UPDATE/DELETE عبر الـ index planner · version GC ·
OPFS للـ playground · group commit · JOIN وGROUP BY.
