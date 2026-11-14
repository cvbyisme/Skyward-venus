// Pure userspace packing; these objects are never submitted to DRM here.
#pragma once
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <stdexcept>
#include <vector>

namespace venus_display {
// Byte layouts are the matching Venus msm_drm_pp.h UAPI, not generic KMS LUTs.
struct IgcBlob {
  uint64_t flags = 0;
  uint32_t c[3][256]{};
  uint32_t strength = 0, last[3]{};
};
struct GcBlob { uint64_t flags = 0; uint32_t c[3][512]{}; };
struct GamutColumn { uint32_t c2_c1 = 0, c0 = 0; };
struct GamutBlob {
  uint64_t flags = 0;
  uint32_t mode = 0, scale_off[3][16]{};
  GamutColumn col[4][1229]{};
};
struct SdeGamut {
  uint32_t mode = 1, map_en = 0, tbl_size[4]{};
  uint32_t *c0_data[4]{}, *c1_c2_data[4]{};
  uint32_t tbl_scale_off_sz[3]{};
  uint32_t *scale_off_data[3]{};
};
struct OwnedSdeGamut {
  SdeGamut cfg;
  uint32_t c0[4][1229]{}, c1_c2[4][1229]{}, scale[3][16]{};
  OwnedSdeGamut() {
    for (size_t i=0; i<4; ++i) {
      cfg.tbl_size[i]=1229; cfg.c0_data[i]=c0[i]; cfg.c1_c2_data[i]=c1_c2[i];
    }
    for (size_t i=0; i<3; ++i) { cfg.tbl_scale_off_sz[i]=16; cfg.scale_off_data[i]=scale[i]; }
  }
  OwnedSdeGamut(const OwnedSdeGamut&) = delete;
  OwnedSdeGamut& operator=(const OwnedSdeGamut&) = delete;
};
static_assert(sizeof(IgcBlob)==3096 && offsetof(IgcBlob,last)==3084);
static_assert(sizeof(GcBlob)==6152 && sizeof(GamutBlob)==39536);
static_assert(offsetof(GamutBlob,col)==204);
static_assert(sizeof(SdeGamut)==128 && offsetof(SdeGamut,scale_off_data)==104);
inline void check_channel(const std::vector<uint32_t>& c, size_t n, uint32_t max) {
  if (c.size()!=n) throw std::runtime_error("unexpected gamma sample count");
  for (uint32_t x:c) if(x>max) throw std::runtime_error("gamma sample exceeds hardware width");
}
inline IgcBlob igc(const std::vector<uint32_t>& r,const std::vector<uint32_t>& g,
                   const std::vector<uint32_t>& b,bool dither,uint32_t strength) {
  check_channel(r,257,4095); check_channel(g,257,4095); check_channel(b,257,4095);
  if(strength>15) throw std::runtime_error("IGC strength exceeds four bits");
  IgcBlob out; out.flags=dither?1:0; out.strength=strength;
  // Vendor ConvertIgcV30Coeff and ConvertGCToPPFeature use hardware G,B,R
  // channel order. Passing R,G,B here would silently alter calibrated colors.
  const std::vector<uint32_t>* cs[]={&g,&b,&r};
  for(size_t c=0;c<3;++c) { for(size_t i=0;i<256;++i) out.c[c][i]=(*cs[c])[i]; out.last[c]=(*cs[c])[256]; }
  return out;
}
inline GcBlob gc(const std::vector<uint32_t>& r,const std::vector<uint32_t>& g,
                 const std::vector<uint32_t>& b) {
  check_channel(r,1024,1023); check_channel(g,1024,1023); check_channel(b,1024,1023);
  GcBlob out; const std::vector<uint32_t>* cs[]={&g,&b,&r};
  for(size_t c=0;c<3;++c) for(size_t i=0;i<512;++i) out.c[c][i]=(*cs[c])[2*i]|((*cs[c])[2*i+1]<<16);
  return out;
}
inline GamutBlob gamut(const OwnedSdeGamut& owned) {
  const auto& cfg=owned.cfg;
  // Initial prototype accepts only the full 17^3 grid produced by Venus STC.
  if(cfg.mode!=1 || cfg.map_en>1) throw std::runtime_error("unsupported gamut mode");
  GamutBlob out; std::memset(static_cast<void*>(&out),0,sizeof(out));
  out.mode=1; out.flags=cfg.map_en?1:0;
  for(size_t t=0;t<4;++t) {
    if(cfg.tbl_size[t]!=1229 || cfg.c0_data[t]!=owned.c0[t] || cfg.c1_c2_data[t]!=owned.c1_c2[t])
      throw std::runtime_error("vendor changed owned gamut buffer bounds");
    for(size_t i=0;i<1229;++i) out.col[t][i]={owned.c1_c2[t][i],owned.c0[t][i]};
  }
  for(size_t t=0;t<3;++t) {
    if(cfg.tbl_scale_off_sz[t]!=16 || cfg.scale_off_data[t]!=owned.scale[t])
      throw std::runtime_error("vendor changed owned gamut scale bounds");
    if(cfg.map_en) for(size_t i=0;i<16;++i) out.scale_off[t][i]=owned.scale[t][i];
  }
  return out;
}
}
