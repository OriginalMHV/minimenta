//! The Windows binary loads the Recycle Bin DLLs only when it uses them.
#![cfg(all(windows, target_env = "msvc"))]

fn u16_at(b: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes(b[at..at + 2].try_into().unwrap()))
}

fn u32_at(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

/// The DLL names in the import table and in the delay-load import table of
/// a PE32+ image, in lowercase.
fn imported_dlls(image: &[u8]) -> (Vec<String>, Vec<String>) {
    let pe = u32_at(image, 0x3C);
    assert_eq!(&image[pe..pe + 4], b"PE\0\0");
    let sections = u16_at(image, pe + 6);
    let optional = pe + 24;
    assert_eq!(u16_at(image, optional), 0x20B, "a PE32+ image");
    let section_table = optional + u16_at(image, pe + 20);
    let offset = |rva: usize| {
        (0..sections)
            .map(|i| section_table + 40 * i)
            .find_map(|s| {
                let (size, start, raw) = (
                    u32_at(image, s + 8),
                    u32_at(image, s + 12),
                    u32_at(image, s + 20),
                );
                (start..start + size)
                    .contains(&rva)
                    .then(|| rva - start + raw)
            })
            .unwrap()
    };
    let name = |rva: usize| {
        let at = offset(rva);
        let len = image[at..].iter().position(|&c| c == 0).unwrap();
        String::from_utf8_lossy(&image[at..at + len]).to_lowercase()
    };
    // Data directory 1 holds 20-byte import descriptors with the name at 12.
    // Data directory 13 holds 32-byte delay-load descriptors with the name at
    // 4. A descriptor with name 0 ends each table.
    let table = |index: usize, stride: usize, name_at: usize| {
        let rva = u32_at(image, optional + 112 + 8 * index);
        if rva == 0 {
            return Vec::new();
        }
        let start = offset(rva);
        (0..1024)
            .map(|i| u32_at(image, start + stride * i + name_at))
            .take_while(|&n| n != 0)
            .map(name)
            .collect()
    };
    (table(1, 20, 12), table(13, 32, 4))
}

#[test]
fn the_recycle_bin_dlls_load_only_when_used() {
    let image = std::fs::read(env!("CARGO_BIN_EXE_minimenta")).unwrap();
    let (imports, delayed) = imported_dlls(&image);
    assert!(imports.iter().any(|d| d == "kernel32.dll"), "{imports:?}");
    for dll in ["shell32.dll", "ole32.dll"] {
        assert!(!imports.iter().any(|d| d == dll), "{dll} in {imports:?}");
        assert!(delayed.iter().any(|d| d == dll), "{dll} not in {delayed:?}");
    }
}
