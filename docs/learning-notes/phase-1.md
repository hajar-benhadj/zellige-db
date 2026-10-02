# ملاحظات التعلم — المرحلة 1: الـ Pager

## شنو درنا؟

بنينا أول طبقة حقيقية فالمحرك: **Pager** — الوحيد اللي كيقرا ويكتب من الملف.
كلشي فوقه (B+Tree، الجداول، SQL) كيشوف "صفحات" ماشي bytes.

## تنسيق الصفحة (page layout)

كل صفحة 4096 bytes بالضبط:

```text
┌────────┬────────┬─────────┬─────────┬──────────────────┬─────────┐
│ type   │ id     │ next    │ reserved│ payload (4076)   │ CRC32   │
│ 4 B    │ 4 B    │ 4 B     │ 4 B     │                  │ 4 B     │
└────────┴────────┴─────────┴─────────┴──────────────────┴─────────┘
```

- **CRC32 فالآخر** كيغطي كلشي قبله: إلا تبدل بايت واحد فالديسك، القراءة
  كتفشل بخطأ واضح `ChecksumMismatch` — ماشي داتا خايبة بصمت. هادي فلسفة
  المحرك كامل: **الكشف المبكر، ماشي الانتشار الصامت**.
- **little-endian**: أصغر byte أول — نفس ترتيب x86/ARM.
- حقل `reserved` غادي يكون لـ WAL LSN فالمرحلة 3 — حجزناه دابا باش
  التنسيق ما يتبدلش.

## مفاهيم Rust الجديدة (بأمثلة من الكود)

### 1. Ownership & borrowing

```rust
pub fn write_page(&mut self, page: &mut Page) -> Result<(), DbError>
```

- `&mut self` = الـ Pager كيتبدل (الـ seek position مثلاً) — وقاعدة
  Rust: **ما يمكنش يكون جوج borrow متزامنين**، يعني مستحيل كود يكتب
  الصفحة ويقراها فنفس الوقت. الأخطاء كتنكشف فالـ compile.
- `Page` كيتتنقل **بالقيمة** (move): `alloc_page` كترجع Page جديدة،
  والملكية كتتنقل للـ caller. ما كاينش garbage collector حيت
  الـ compiler كيعرف بالضبط فين كتموت كل قيمة.

### 2. Result و `?`

```rust
self.file.seek(SeekFrom::Start(offset))?;
```

`?` = "إلا كان خطأ، رجعه لفوق؛ إلا لا، خذ القيمة". بلا exceptions،
بلا null — الأخطاء جزء من نوع القيمة.

### 3. القاعدة ديال byte fiddling

```rust
let mut b = [0u8; 4];
b.copy_from_slice(&self.bytes[off..off + 4]);
u32::from_le_bytes(b)
```

علاش ماشي مباشر؟ حيت slice → array خاصو نسخة مؤكدة الطول. Rust كيفرض
أنك تتعامل مع "ما إلا كان الـ slice أصغر من 4" — الأمان قبل الاختصار.

## هندسة موجود فالكود (شوفوها مزيان)

1. **`seal()` مخفي على الـ caller**: `write_page` هي اللي كتطبّع الـ
   checksum. مستحيل تكتب صفحة بلا checksum — الخطأ البشري مستحيل بالنظام
   التصميمي، ماشي بالانضباط.
2. **double free guard**: `free_page` كتقرا الصفحة قبل ما ترجعها
   للـ free list — الثمن: قراءة زايدة. الفائدة: خطأ صريح بلاصة
   free list معطوبة بصمت. (الأداء يتقاس فالمرحلة 7، الصح ديما أولاً.)
3. **meta page (صفحة 0)**: الملف كيشرح راسو بوحدو — magic `ZDB1`، عدد
   الصفحات، راس الـ free list. بلا ملفات metadata خارجية.

## الاختبارات اللي كتحمينا

- `detects_a_byte_flipped_between_sessions`: كنفسرو الملف مباشرة
  (بلا pager) ونقلب بايت — القراءة الخاصة خاصها تفشل. هادي هي
  المحاكاة الحقيقية للـ torn write.
- `free_list_recycles_pages_lifo`: الترتيب LIFO كيبقى الصفحات
  المعاد استعمالها قريبة من الـ cache.
- `pages_survive_reopen` (integration): 50 صفحة بنمط معروف، إغلاق،
  فتح جديد، تحقق byte بـ byte — أقرب حاجة لمحاكاة restart.

## الخطوة الجاية (المرحلة 2)

B+Tree فوق الـ pager: splits, merges, range scans — و first contact
مع **property-based testing** ضد `BTreeMap` كمرجع.
