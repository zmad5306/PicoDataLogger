# CYW43439 firmware blobs

These files support the CYW43439 Wi-Fi companion chip on the Raspberry Pi Pico 2 W.

## Provenance

Source repository: [embassy-rs/embassy](https://github.com/embassy-rs/embassy/tree/cf7d3915b4d36c4ea0b453f26c4dc3c362245d6c/cyw43-firmware)

Pinned revision:

```text
cf7d3915b4d36c4ea0b453f26c4dc3c362245d6c
```

The files are redistributed under the included Infineon Permissive Binary License.

## Files

| File | Purpose | Size | SHA-256 |
| --- | --- | ---: | --- |
| `43439A0.bin` | CYW43439 Wi-Fi firmware | 231077 bytes | `5555e0261da2610a500d68c18d895cace0152bbefbf76f4aa683ebce77e3d7eb` |
| `43439A0_clm.bin` | Country locale matrix data | 984 bytes | `e712b3d218e8b1e2747b092e03b8b0afcb8c8c8e355d2a4a0d47b493800f3f89` |
| `nvram_rp2040.bin` | Pico W CYW43439 board configuration | 742 bytes | `4904bdbb0c937bd0ac2eb2a1d62f2da4dd90e32082384e02874e8d671b0f330d` |
| `LICENSE-permissive-binary-license-1.0.txt` | Firmware redistribution license | 2419 bytes | `5f65b8a496ac27afda41917c18cb6e690b4a022df1f5a12ea823eb38a287f50e` |

Verify the files on macOS or Linux with:

```bash
shasum -a 256 firmware/*.bin firmware/LICENSE-permissive-binary-license-1.0.txt
```

The `nvram_rp2040.bin` filename comes from the upstream Pico W configuration. It configures the CYW43439 module and is also used by Embassy’s Pico W example.