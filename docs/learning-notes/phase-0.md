# ملاحظات التعلم — المرحلة 0: التأسيس

> هاد الملف كيتشرح بالدارجة باش يسهل المتابعة. الكود والوثائق الرسمية بالإنجليزية.

## شنو درنا فهاد المرحلة؟

صاوبنا **Cargo Workspace**: مشروع واحد فيه عدة crates (حزم) مترابطة:

```
zellige-db/
├── Cargo.toml          ← جذر الـ workspace: كيعرّف الأعضاء والإعدادات المشتركة
└── crates/
    ├── zdb-core/       ← طبقة التخزين (pager, btree, wal)
    ├── zdb-sql/        ← طبقة SQL (parser, planner, executor)
    └── zdb-server/     ← البرنامج النهائي (CLI + سيرفر)
```

**علاش workspace ماشي مشروع واحد؟** حيت كيفرض حدود نظيفة: `zdb-sql` ما يقدرش
يستعمل تفاصيل داخلية من `zdb-core` إلا كانت public — وهذا كيجبرنا على تصميم
واجهات واضحة، بحال المشاريع الحقيقية.

## المفاهيم الجديدة

- **Crate** = وحدة تجميع فـ Rust (library ولا binary). `zdb-server` فيه `[[bin]]`
  يعني كينتج أمر اسمو `zdb`.
- **Edition 2024** = نسخة "القواعد" ديال اللغة. كتتغير كل ~3 سنين؛ الرست
  ديالك 1.99 كيسنolah.
- **`#![deny(unsafe_code)]`** = أمر صارم على مستوى الـ crate: أي `unsafe` =
  خطأ compilation. عندنا ما محتاجينهش أصلاً، وهذا وعد للقارئ.
- **CI (GitHub Actions)** = كل push كيتشاف أوتوماتيكياً على 3 أنظمة (Linux,
  Windows, macOS) مع `fmt` (التنسيق) و `clippy` (التحذيرات) و `tests`.
  قاعدة ذهبية: **CI أخضر ولا المشروع مكسور.**

## الأوامر اللي غادي نستعملو كل يوم

```bash
cargo build            # ترجمة
cargo test             # الاختبارات
cargo clippy --workspace --all-targets -- -D warnings   # lint صارم
cargo fmt              # تنسيق تلقائي
cargo run -p zdb-server # تشغيل الـ CLI
```

> ملاحظة Windows: إلا `cargo` ما تلقاش فالطرفية، زيد `~/.cargo/bin` للـ PATH
> (`export PATH="$HOME/.cargo/bin:$PATH"` فـ Git Bash).

## Conventional Commits

الرسائل ديال git عندنا تنسيق ثابت:

```
feat(pager): add CRC32 verification      # ميزة جديدة
fix(btree): handle underflow in merge    # إصلاح
test(wal): crash-recovery fuzz harness   # اختبارات
docs: add ADR-0002                       # وثائق
chore(ci): add windows runner            # صيانة
```

`type(scope): description` — فالبورتفوليو هادشي كيبين انضباط هندسي.

## خطوة خطوة فالقادم

فالمرحلة 1 غادي نشوفو أول مفاهيم Rust العميقة: **ownership** (من كيملك
الذاكرة)، **borrowing** (`&` و `&mut`)، و**Result** (معالجة الأخطاء بلا
exceptions). كل واحد فيهم عندو مثال مباشر من كود الـ pager.
