#pragma once
#include <array>
#include <cstdint>
#include <cstring>
#include <stdexcept>
namespace display_ipc {
constexpr size_t size=64;
constexpr uint16_t hello=1,request=2,reply=3;
constexpr uint32_t ping=1,pause=2,resume=3,ready=0,paused=1,failed=2;
struct Message {
 uint16_t kind=0; uint64_t seq=0,generation=0,commit=0;
 uint32_t op=0; int32_t status=0; uint32_t width=0,height=0,state=0;
};
inline void check(bool ok,const char* why){if(!ok)throw std::runtime_error(why);}
inline void put(uint8_t* p,uint64_t n,unsigned bytes){for(unsigned i=0;i<bytes;i++)p[i]=static_cast<uint8_t>(n>>(8*i));}
inline uint64_t get(const uint8_t* p,unsigned bytes){uint64_t n=0;for(unsigned i=0;i<bytes;i++)n|=uint64_t(p[i])<<(8*i);return n;}
inline std::array<uint8_t,size> encode(const Message& m){std::array<uint8_t,size>b{};std::memcpy(b.data(),"VDI1",4);
 put(&b[4],1,2);put(&b[6],m.kind,2);put(&b[8],m.seq,8);put(&b[16],m.generation,8);put(&b[24],m.commit,8);
 put(&b[32],m.op,4);put(&b[36],static_cast<uint32_t>(m.status),4);put(&b[40],m.width,4);put(&b[44],m.height,4);put(&b[48],m.state,4);return b;}
inline Message decode(const uint8_t* b,size_t n){check(n==size,"wrong packet length");check(!std::memcmp(b,"VDI1",4)&&get(b+4,2)==1,"wrong packet magic/version");
 for(size_t i=52;i<size;i++)check(b[i]==0,"nonzero packet reserved byte");
 return {static_cast<uint16_t>(get(b+6,2)),get(b+8,8),get(b+16,8),get(b+24,8),static_cast<uint32_t>(get(b+32,4)),static_cast<int32_t>(get(b+36,4)),static_cast<uint32_t>(get(b+40,4)),static_cast<uint32_t>(get(b+44,4)),static_cast<uint32_t>(get(b+48,4))};}
inline void validate_reply(const Message& m,const Message& sent,const Message& previous){
 check(m.kind==reply && m.seq==sent.seq && m.generation==sent.generation && m.op==sent.op && m.status==0,"owner rejected or mismatched reply");
 check(m.width==previous.width && m.height==previous.height && m.commit>=previous.commit,"owner geometry or commit regressed");
 check(sent.op!=pause || m.state==paused,"pause was not acknowledged");
 check(sent.op!=resume || (m.state==ready && m.commit>previous.commit),"resume has no completed refresh");
 check(m.state==ready || m.state==paused,"owner is failed");
}
}
