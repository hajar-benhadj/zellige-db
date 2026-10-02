# ملاحظات التعلم — المرحلة 7: Benchmarks والـ Page Cache

## المبدأ: الأرقام ماشي الصفات

"سريع" و "فعال" كلمات فارغة فالبورتفوليو. "132x أسرع بـ index scan"
جملة كيتحقق منها أي واحد. كل رقم هنا قابل للتكرار: `cargo bench`.

## قصة الـ Optimization: Page Cache

**المشكل المقاس**: كل عملية صفحة = جوج syscalls (seek + read/write).
الـ insert كان ~55μs — الأغلبية الساحقة syscalls ماشي CPU.

**الحل (40 سطر)**: `HashMap<PageId, Page>` داخل الـ pager، سقف 8192
صفحة (32MB)، eviction عشوائي (الكاش accelerator صافي — السياسة
متعمدة مملة).

**النتيجة المقاسة**:
- insert: 55 → 42 μs/op (−24%)
- point get: 36 → 26 μs/op (−30%)
- range scan: 15 → 10 μs/op (−33%)

## حكاية الـ bug الذهبية

أول تشغيل للـ crash tests بعد إضافة الكاش:

```
Error: PageOutOfBounds(4)
```

السبب: `write_page_raw` (اللي كيستعملو recovery) كتب الصفحة المصلحة
فالديسك **بلا ما يحدث الكاش** → reload_meta قرا النسخة القديمة من
الكاش → recovery فشل على داتا سليمة.

الدرسين:
1. **Cache invalidation هو أصل المشاكل** (قول Phil Karlton الشهير).
2. **الـ randomized crash tests صطادو الـ bug فأول تشغيل** — هادا
   هو العائد ديال الاختبارات. الإصلاح سطرين، والاختبار بقا حارس
   للأبد.

## قراءة الأرقام بصدق

| قياس | النتيجة | التفسير |
|---|---|---|
| auto-commit INSERT | ~2.6ms | fsync لكل statement = ثمن الدبرabilité |
| batched INSERT | ~330μs | **7.8x** — نفس الضمانة موزعة على الباكاتش |
| full scan (10k) | ~87ms | O(n) لكل query |
| index scan | ~660μs | **132x** — O(log n + matches) |

**نقطة صدق**: UPDATE مازال كيدير full scan (الـ planner كيغطي SELECT
ف v0.1) — مكتوبة فالـ README كـ known bottleneck، ماشي مخبية.

## بدون criterion (اختيار موثق فـ ADR-0008)

criterion كيتطلب binutils كاملين (raw-dylib deps) — مخاطر build ف
windows-gnu. الحل: `[[bench]] harness = false` = برنامج عادي
بـ `std::time::Instant`، portable، صفر dependencies. criterion
يقدر يدخل من بعد خلف نفس الـ targets.

## للنساء والرجال اللي غادي يقيسو: قواعد

1. release build ديما (`cargo bench` كيديرها)
2. تشغيلتين على الأقل، سجّل الـ median
3. RATIOS ماشي أرقام مطلقة — الجهاز ديالي ماشي الجهاز ديالك
4. وثّق البيئة (OS, build profile, warm cache)
