use std::fs;
use std::path::Path;
use std::process::Command;

#[tauri::command]
pub async fn patch_agy_binary(file_path: String) -> Result<String, String> {
    let mut actual_path = file_path.clone();
    if actual_path.ends_with(".app") || actual_path.ends_with(".app/") {
        let app_path = Path::new(&actual_path);
        let inner = app_path.join("Contents/MacOS/agy");
        if inner.exists() {
            actual_path = inner.to_string_lossy().to_string();
        }
    }

    let path = Path::new(&actual_path);
    if !path.exists() {
        return Err("File not found".into());
    }

    let data = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let n = data.len();
    let mut patch_offset = None;
    let mut new_inst_bytes = None;

    // Scan for eligibility gate pattern (ARM64)
    for i in (0..n - 20).step_by(4) {
        let inst1 = u32::from_le_bytes(data[i..i+4].try_into().unwrap());
        let inst2 = u32::from_le_bytes(data[i+4..i+8].try_into().unwrap());
        let inst4 = u32::from_le_bytes(data[i+12..i+16].try_into().unwrap());
        let inst5 = u32::from_le_bytes(data[i+16..i+20].try_into().unwrap());

        // 1. ldrb wA, [xB, #0x58]
        if (inst1 & 0xfffffc00) != 0x39416000 {
            continue;
        }
        let b_reg = (inst1 >> 5) & 0x1f;
        let a_reg = inst1 & 0x1f;

        // 2. tbnz wA, #0, label1
        if (inst2 & 0xffe0001f) != (0x37000000 | a_reg) {
            continue;
        }

        // 3. ldr xC, [xB, #0x38]
        if (inst4 & 0xfffffc00) != 0xf9401c00 || ((inst4 >> 5) & 0x1f) != b_reg {
            continue;
        }
        let c_reg = inst4 & 0x1f;

        // 4. cbz xC, label_send
        if (inst5 & 0xffe0001f) != (0xb4000000 | c_reg) {
            continue;
        }

        // Extract imm19 from cbz
        let imm19_raw = (inst5 >> 5) & 0x7ffff;
        let imm19 = if (imm19_raw & 0x40000) != 0 {
            (imm19_raw as i32) - 0x80000
        } else {
            imm19_raw as i32
        };

        patch_offset = Some(i + 16);
        // Encode unconditional branch: b label_send (0x14000000 | (imm19 & 0x3ffffff))
        let b_inst = 0x14000000 | ((imm19 as u32) & 0x3ffffff);
        new_inst_bytes = Some(b_inst.to_le_bytes());
        break;
    }

    if patch_offset.is_none() {
        // Check if already patched
        for i in (0..n - 20).step_by(4) {
            let inst1 = u32::from_le_bytes(data[i..i+4].try_into().unwrap());
            let inst2 = u32::from_le_bytes(data[i+4..i+8].try_into().unwrap());
            let inst4 = u32::from_le_bytes(data[i+12..i+16].try_into().unwrap());
            let inst5 = u32::from_le_bytes(data[i+16..i+20].try_into().unwrap());

            if (inst1 & 0xfffffc00) == 0x39416000 {
                let b_reg = (inst1 >> 5) & 0x1f;
                let a_reg = inst1 & 0x1f;
                if (inst2 & 0xffe0001f) == (0x37000000 | a_reg) {
                    if (inst4 & 0xfffffc00) == 0xf9401c00 && ((inst4 >> 5) & 0x1f) == b_reg {
                        // If inst5 is already `b label_send` (0x14000000)
                        if (inst5 & 0xfc000000) == 0x14000000 {
                            return Ok("Binary is already patched.".into());
                        }
                    }
                }
            }
        }
        return Err("Pattern not found. This version of the CLI might not have the eligibility gate, or the structure has changed.".into());
    }

    let offset = patch_offset.unwrap();
    let patch_bytes = new_inst_bytes.unwrap();

    // Create backup
    let backup_path = format!("{}.bak", actual_path);
    if !Path::new(&backup_path).exists() {
        fs::copy(path, &backup_path).map_err(|e| format!("Failed to create backup: {}", e))?;
    }

    // Apply patch
    use std::io::{Seek, SeekFrom, Write};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("Failed to open file for writing: {}", e))?;
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|e| format!("Seek failed: {}", e))?;
    file.write_all(&patch_bytes)
        .map_err(|e| format!("Write failed: {}", e))?;

    // Re-sign on macOS
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("codesign")
            .args(&["--remove-signature", &actual_path])
            .output();
        let output = Command::new("codesign")
            .args(&["--sign", "-", &actual_path])
            .output();
        match output {
            Ok(out) if out.status.success() => {},
            Ok(out) => {
                let err_msg = String::from_utf8_lossy(&out.stderr);
                return Err(format!("Patch applied, but codesigning failed: {}", err_msg));
            },
            Err(e) => return Err(format!("Patch applied, but codesigning execution failed: {}", e)),
        }
    }

    Ok("Patch applied successfully!".into())
}
