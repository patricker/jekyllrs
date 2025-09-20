# frozen_string_literal: true

require "rbconfig"

namespace :rust do
  # Build the Rust native bridge and export JEKYLL_RUST_LIB for subsequent tasks
  desc "Build Rust native bridge (RUST_PROFILE=debug|release) and set JEKYLL_RUST_LIB"
  task :build do
    root = File.expand_path("..", __dir__)
    profile = ENV["RUST_PROFILE"] == "release" ? "release" : "debug"

    sh File.join(root, "script", "rust-build")

    exts = [RbConfig::CONFIG["DLEXT"], "so", "dylib", "bundle"].compact.uniq
    lib_path = nil
    exts.each do |ext|
      candidate = File.join(root, "rust", "target", profile, "libjekyll_core.#{ext}")
      if File.exist?(candidate)
        lib_path = candidate
        break
      end
    end

    abort "Rust build completed but no jekyll_core library was found in target/#{profile}" unless lib_path

    ENV["JEKYLL_RUST_LIB"] = lib_path
    puts "JEKYLL_RUST_LIB=#{lib_path}"
  end

  desc "Run Ruby unit tests against the Rust bridge"
  task :test => :build do
    Rake::Task["test"].invoke
  end
end

namespace :build do
  # Convenience task: build Rust bridge first, then package the gem
  desc "Build gem after building Rust (sets JEKYLL_RUST_LIB)"
  task :with_rust => ["rust:build", :build]
end

namespace :features do
  desc "Run Cucumber against the Rust bridge (expensive; auto-rebuilds Rust)"
  task :rust do
    root = File.expand_path("..", __dir__)
    sh File.join(root, "scripts", "rustycucumber")
  end
end
