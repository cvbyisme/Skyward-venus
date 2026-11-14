# Sources and ABI evidence

Public flow and original interface declarations:

- [AOSP Qualcomm sm7250 STC interface](https://android.googlesource.com/platform/hardware/qcom/sm7250/display/+/f7f00a9e30a7b9b0f36997ac3830f4a326525dea/sdm/include/private/snapdragon_color_intf.h)
- [AOSP color manager](https://android.googlesource.com/platform/hardware/qcom/sm7250/display/+/f7f00a9e30a7b9b0f36997ac3830f4a326525dea/sdm/libs/core/color_manager.cpp)
- [AOSP DRM hardware conversion](https://android.googlesource.com/platform/hardware/qcom/sm7250/display/+/f7f00a9e30a7b9b0f36997ac3830f4a326525dea/sdm/libs/core/drm/hw_color_manager_drm.cpp)

The matching Qualcomm BSD notice is retained in `LICENSE-Qualcomm`. This code
uses the public interface declarations and packing flow, with the following
explicit Venus V2 adjustments verified from the matched vendor binaries.
The public header alone is insufficient for this phone.

The vendor is Xiaomi Venus OS2.0.2.0.UKBCNXM, matching all 35 current-device
hashes in `vendor-pins.json`. Original extracted files live at
`/home/abc/other/venus-stock-analysis-20261030/extracted/vendor`; current-device
hash evidence is `venus_dump/network-refine-20261111/display-live-vendor-hashes.txt`.
`.gnu_debugdata` supplied symbol names, not DWARF structure definitions. Layout
claims below use actual field loads, size guards, construction and destruction.

| Contract | Evidence in matched ELF |
|---|---|
| Factory V2 `(2,0)` returns raw interface, V1 is separate default implementation | manager `0x11d84`, exported jump `0x20760` |
| Vtable Init/DeInit/Set/Get/Process at 0x10/18/20/28/30 after destructors | core UpdateModeHwassets, manager calls; public interface order |
| ScPayload 24 B; len@4, prop@8, pointer@16 | manager `0x1675c`; core constants `0x28150` = `(24,32)`, `0x281a8` = `(2912,3)` |
| ColorMode 64 B, added string@40; ColorModeList version = 32+64 | manager GetModesList `0x125a4`, VersionCheck `0x15a8c` |
| ModeRenderInputParams 2912 B; vector@8, mode@32, metadata flag@96, opaque metadata@100 (2804 B), ID@2904 | core UpdateModeHwassets `0x7c078` |
| OutputParams 32 B; payload entries 56 B, string@8, shared_ptr@32, len@48 | same core and manager render/destructors |
| Gamma 88 B; enabled@4, vectors@8/32/56, dither@80/84 | qdcm ConvertPostBlendInvGammaPayload `0xd5a4` |
| Gamut 117936 B with inline 117928 B LUT at offset 8 | qdcm ConvertPostBlendGamutPayload `0xd3a0` |
| IGC 257 entries, GC 1024, full gamut 17-grid | manager constants `0xe4a0`, `0xe4b8`, `0xe4d0` |
| Hardware G/B/R channel order | color ConvertGCToPPFeature `0x3d99c`; ConvertIgcV30Coeff `0x49d44` |
| IGC V4 includes last samples and dither; size 3096 | core `0xa42f0`, last stores `0xa4424` / `0xa4434` / `0xa4440` |
| Gamut coefficient conversion is pure static two-pointer function | color exported `0x620e0` -> `0x489dc`; helper writes bounded SDE buffers |
| Gamut blob 39536 B | core GetDrmGamut `0xa4770`; current kernel UAPI |

Readable disassemblies and official source snapshots are under
`venus_dump/network-refine-20261111/display-abi/` and `display-reference/sm7250/`.
No proprietary binary or calibration table is copied into the source directory.

The read-only kernel UAPI source is
`/home/abc/android_kernel_xiaomi_sm8350_venus/techpack/display/include/uapi/display/drm/msm_drm_pp.h`.
The current panel is `qcom,mdss_dsi_k2_38_08_0a_mp_dsc_cmd`; the compositor's
connector capability string is `xiaomi 38 08 0a cmd mode dsc dsi panel`.
SDM `PPHWAttributes::Set` replaces ASCII spaces with underscores and cuts at
the first newline **before** `STCIntfClient::Init(hw_attr.panel_name)`.
The identifier consumed by STC is therefore
`xiaomi_38_08_0a_cmd_mode_dsc_dsi_panel`. The matching vendor core confirms
the same operations at `0x7a7d4..0x7a808` (space `0x20` to `_` `0x5f`, then
newline `0x0a` to NUL). The corrected probe follows this caller contract;
it does not rename or substitute any calibration XML. The original spaced
argument caused ENOENT on all QDCM calibration paths in the first trace.
