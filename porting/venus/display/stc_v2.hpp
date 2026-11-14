// Experimental Venus vendor ABI, verified against pinned OS2.0.2.0 ELFs.
// Interface ordering/field semantics follow the BSD-licensed Qualcomm header
// listed in SOURCES.md; V2 changes are from the matching vendor call sites.
#pragma once
#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace snapdragoncolor {
enum ScProperty : uint32_t { kInvalid, kModeList, kNeedsUpdate,
  kModeRenderInputParams, kHwConfigPayloadParam };
enum ScOps : uint32_t { kScModeRenderIntent };
struct ScPayload {
  uint32_t version = sizeof(ScPayload), len = 0;
  ScProperty prop = kInvalid;
  uint64_t payload = 0;
};
class ScPostBlendInterface {
 public:
  virtual ~ScPostBlendInterface() = default;
  virtual int Init(const std::string&) = 0;
  virtual int DeInit() = 0;
  virtual int SetProperty(const ScPayload&) = 0;
  virtual int GetProperty(ScPayload*) = 0;
  virtual int ProcessOps(ScOps, const ScPayload&, ScPayload*) = 0;
};
struct ColorMode {
  uint32_t gamut = 0xff, gamma = 0xff, intent = 0xffff;
  std::vector<std::string> hw_assets;
  std::string name;  // V2 addition at offset 40, absent in public V1.
};
struct ColorModeList {
  uint32_t version = sizeof(ColorModeList) + sizeof(ColorMode);
  std::vector<ColorMode> list;
};
struct HwConfigPayload {
  uint32_t version = sizeof(HwConfigPayload);
  std::string hw_asset;
  std::shared_ptr<void> hw_payload;
  uint32_t hw_payload_len = 0;
};
struct HwConfigOutputParams {
  uint32_t version = sizeof(HwConfigOutputParams);
  std::vector<HwConfigPayload> payload;
};
struct ModeRenderInputParams {
  uint32_t version = sizeof(ModeRenderInputParams);
  std::vector<std::string> hw_assets_for_mode;
  ColorMode color_mode;
  bool valid_meta_data = false;
  // SDR only. Never interpret or pass valid HDR metadata using this opaque ABI.
  alignas(4) std::array<uint8_t, 2804> meta_data{};
  int32_t mode_id = -1;
};
struct GammaPostBlendConfig {
  uint32_t version;
  bool enabled;
  std::vector<uint32_t> r, g, b;
  bool dither_enabled;
  uint32_t dither_strength;
};
struct GamutConfig {
  uint32_t version;
  bool enabled;
  alignas(8) std::array<uint8_t, 117928> gamut_info;
};
static_assert(sizeof(void*) == 8 && sizeof(std::string) == 24,
              "Requires ARM64 platform libc++ std::__1 ABI");
static_assert(sizeof(ScPayload) == 24 && offsetof(ScPayload,payload) == 16);
static_assert(sizeof(ColorMode) == 64 && offsetof(ColorMode,name) == 40);
static_assert(sizeof(ColorModeList) == 32 && offsetof(ColorModeList,list) == 8);
static_assert(sizeof(HwConfigPayload) == 56 && offsetof(HwConfigPayload,hw_payload) == 32);
static_assert(sizeof(HwConfigOutputParams) == 32);
static_assert(sizeof(ModeRenderInputParams) == 2912);
static_assert(offsetof(ModeRenderInputParams,color_mode) == 32);
static_assert(offsetof(ModeRenderInputParams,valid_meta_data) == 96);
static_assert(offsetof(ModeRenderInputParams,meta_data) == 100);
static_assert(offsetof(ModeRenderInputParams,mode_id) == 2904);
static_assert(sizeof(GammaPostBlendConfig) == 88);
static_assert(offsetof(GammaPostBlendConfig,r) == 8);
static_assert(offsetof(GammaPostBlendConfig,dither_enabled) == 80);
static_assert(sizeof(GamutConfig) == 117936 && offsetof(GamutConfig,gamut_info) == 8);
template<class T> ScPayload wrap(ScProperty prop, T& value) {
  ScPayload p; p.len=sizeof(value); p.prop=prop;
  p.payload=reinterpret_cast<uint64_t>(&value); return p;
}
}
