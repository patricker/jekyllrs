# frozen_string_literal: true

require "rb_sys/mkmf"

cargo_manifest = File.expand_path("../../rust/jekyll-core/Cargo.toml", __dir__)

create_rust_makefile("jekyll_core", cargo_manifest: cargo_manifest)
