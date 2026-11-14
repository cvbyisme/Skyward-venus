#include "blobs.hpp"
#include <cassert>
#include <cstdio>
#include <cstring>
#include <functional>
#include "msm_drm_pp.h"

using namespace venus_display;
static_assert(sizeof(IgcBlob)==sizeof(drm_msm_igc_lut));
static_assert(sizeof(GcBlob)==sizeof(drm_msm_pgc_lut));
static_assert(sizeof(GamutBlob)==sizeof(drm_msm_3d_gamut));
static_assert(offsetof(GamutBlob,col)==offsetof(drm_msm_3d_gamut,col));
static_assert(offsetof(IgcBlob,last)==offsetof(drm_msm_igc_lut,c0_last));
void rejects(const std::function<void()>& f){bool threw=false;try{f();}catch(const std::runtime_error&){threw=true;}assert(threw);}
int main(){
  std::vector<uint32_t> r(257,100),g(257,200),b(257,300);
  r.back()=111;g.back()=222;b.back()=333;
  auto i=igc(r,g,b,true,7);
  assert(i.flags==IGC_DITHER_ENABLE && i.strength==7);
  assert(i.c[0][0]==200 && i.c[1][255]==300 && i.c[2][1]==100);
  assert(i.last[0]==222 && i.last[1]==333 && i.last[2]==111);
  rejects([&]{auto short_r=r;short_r.pop_back();igc(short_r,g,b,false,0);});
  rejects([&]{auto bad=r;bad[256]=4096;igc(bad,g,b,false,0);});
  rejects([&]{igc(r,g,b,true,16);});
  r.resize(1024,10);g.resize(1024,20);b.resize(1024,30);
  r[1022]=11;r[1023]=12;g[0]=21;g[1]=22;
  auto c=gc(r,g,b);assert(c.c[0][0]==(21u|(22u<<16)));
  assert(c.c[2][511]==(11u|(12u<<16)));
  rejects([&]{auto bad=b;bad[31]=1024;gc(r,g,bad);});
  rejects([&]{auto bad=g;bad.push_back(0);gc(r,bad,b);});
  OwnedSdeGamut owned;owned.cfg.map_en=1;
  owned.c0[3][1228]=0xabc;owned.c1_c2[3][1228]=0x1230456;owned.scale[2][15]=77;
  auto z=gamut(owned);assert(z.col[3][1228].c0==0xabc && z.col[3][1228].c2_c1==0x1230456);
  assert(z.scale_off[2][15]==77 && z.flags==GAMUT_3D_MAP_EN);
  const auto* bytes=reinterpret_cast<const unsigned char*>(&z);
  for(size_t n=sizeof(z)-4;n<sizeof(z);++n)assert(bytes[n]==0); // deterministic tail padding
  owned.cfg.map_en=0;assert(gamut(owned).scale_off[2][15]==0);
  owned.cfg.c0_data[0]=owned.c0[1];rejects([&]{gamut(owned);});
  owned.cfg.c0_data[0]=owned.c0[0];owned.cfg.tbl_size[3]=1230;rejects([&]{gamut(owned);});
  owned.cfg.tbl_size[3]=1229;owned.cfg.mode=2;rejects([&]{gamut(owned);});
  std::puts("PASS: UAPI sizes, channel order, IGC last sample, bounds, ownership, padding");
}
