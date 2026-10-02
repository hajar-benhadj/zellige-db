# ملاحظات التعلم — المرحلة 8: WebAssembly والمتصفح

## الهدف المحقق

> قاعدة بيانات كاملة — pager, B+Tree, WAL, SQL — خدّامة فتاب
> ديال المتصفح، بلا سيرفر، بلا شبكة.

## الحيلة المعمارية: trait `PageFile`

WebAssembly ما عندوش filesystem. المحرك كان مبني مباشرة على
`std::fs::File`. الحل: تجريد بـ 4 طرق فقط:

```rust
trait PageFile {
    fn read_exact_at(...); fn write_all_at(...);
    fn set_len(...);       fn sync(...);
}
```

- `OsFile`: seek + read/write + fsync (كيفما كان قبل)
- `MemoryFile`: `Vec<u8>` — الثقوب كتقرا أصفار بحال sparse file،
  و sync = no-op (الذاكرة هي الوسط الدائم هنا)

`Database::create_memory()` → نفس المحرك بلا ديسك. **ما تبدل حتى
لوغاريتم** — refactor ميكانيكي.

## تعليم wasm-bindgen

- `#[wasm_bindgen]` على struct + impl → JS glue أوتوماتيكي
- **الإصدارات خاصها تتطابق**: wasm-bindgen crate (فـ Cargo.lock:
  0.2.129) مع wasm-bindgen-cli — غير matching version كيقدر يقرا
  الـ .wasm
- الحيلة ديالنا: CLI جاهز من GitHub releases (cargo install كان
  محبوس فـ binutils ناقص فـ windows-gnu)
- **`#[cfg(target_arch = "wasm32")]`** على مودول الـ bindings: الـ host
  build كيولي lib فارغ و `cargo test --workspace` كيبقى أخضر فـ 3
  أنظمة

## التحقق: متصفح حقيقي

المتصفح (عبر automation) فتح الصفحة، سالا الـ wasm boot ("engine
ready ✓")، داز SQL كامل (CREATE + INSERT + SELECT + WHERE) وشاف
جداول ASCII. الصفحة ثابتة على GitHub Pages: كل زائر يجرّب قاعدة
بيانات حقيقية فثانية.

## بنية النشر

```text
cargo build --target wasm32-unknown-unknown  →  zdb_wasm.wasm
wasm-bindgen --target web                    →  pkg/ (js + wasm + d.ts)
web/playground/ (index.html + pkg)           →  GitHub Pages workflow
```

index.html بلا build step: vanilla JS + ESM import. زِرو dependencies
فالواجهة، بحال بحال المحرك.

## الجاي: المرحلة 9 — الإطلاق (CHANGELOG + tag + release + blog)
