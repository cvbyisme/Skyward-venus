// SPDX-License-Identifier: Apache-2.0
#include "locks.h"
#include <cassert>
#include <set>
#include <thread>
#include <vector>

class KernelModel final : public venus_suspend::Operations {
    std::mutex mutex_;
  public:
    std::set<std::string> held;
    int writes=0;
    std::set<std::string> ever;
    bool refuse_acquire=false;
    bool refuse_release=false;
    void acquire(const std::string& token) override {
        std::lock_guard<std::mutex> g(mutex_);
        if (refuse_acquire) throw std::runtime_error("injected acquire error");
        assert(held.insert(token).second); ever.insert(token); ++writes;
    }
    void release(const std::string& token) override {
        std::lock_guard<std::mutex> g(mutex_);
        if (refuse_release) throw std::runtime_error("injected release error");
        assert(held.erase(token)==1); ++writes;
    }
};

int main() {
    using venus_suspend::Lease;
    auto model=std::make_shared<KernelModel>();
    auto tokens=std::make_shared<venus_suspend::Tokens>(1234);
    {
        auto first=std::make_unique<Lease>(model,tokens);
        auto second=std::make_unique<Lease>(model,tokens);
        assert(model->held.size()==2);
        first->release();first->release();
        assert(model->held.size()==1);
        first.reset();assert(model->held.size()==1);
        second.reset();assert(model->held.empty());
        assert(model->writes==4);
    }
    model->refuse_acquire=true;
    bool failed=false;
    try { Lease rejected(model,tokens); } catch (const std::runtime_error&) { failed=true; }
    assert(failed && model->held.empty());
    model->refuse_acquire=false;
    {
        Lease held(model,tokens);
        model->refuse_release=true;failed=false;
        try { held.release(); } catch (const std::runtime_error&) { failed=true; }
        assert(failed && model->held.size()==1);
        model->refuse_release=false;
        // The failed release did not discard ownership: destruction retries.
    }
    assert(model->held.empty());
    {
        Lease shared(model,tokens);
        const int before=model->writes;
        std::vector<std::thread> threads;
        for(int i=0;i<16;++i) threads.emplace_back([&]{shared.release();});
        for(auto& thread:threads) thread.join();
        assert(model->writes==before+1 && model->held.empty());
    }
    const auto before_names=model->ever.size();
    for(int i=0;i<10000;++i) { Lease cycle(model,tokens); }
    assert(model->ever.size()<=before_names+1 && model->held.empty());
    {
        Lease stale(model,tokens); stale.release();
        Lease replacement(model,tokens);
        const auto names=model->held;
        stale.release();
        assert(model->held==names && model->held.size()==1);
    }
    {
        Lease failed_release(model,tokens);
        const auto failed_names=model->held;
        model->refuse_release=true;
        try { failed_release.release(); assert(false); } catch (const std::runtime_error&) {}
        model->refuse_release=false;
        Lease another(model,tokens);
        assert(model->held.size()==2);
        assert(model->held.count(*failed_names.begin())==1);
    }
    const auto concurrent_before=model->ever.size();
    {
        std::vector<std::thread> threads;
        for(int i=0;i<16;++i) threads.emplace_back([&] {
            for(int n=0;n<1000;++n) { Lease cycle(model,tokens); }
        });
        for(auto& thread:threads) thread.join();
    }
    assert(model->held.empty() && model->ever.size()<=concurrent_before+16);
    std::puts("PASS: independent leases, idempotent release, destruction, failure retention, bounded sequential/concurrent slot reuse");
}
