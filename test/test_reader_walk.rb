# frozen_string_literal: true

require "helper"

class TestReaderWalk < JekyllUnitTest
  context "Rust reader_walk recursion" do
    setup do
      @site = fixture_site
    end

    should "return pages and static relative paths recursively" do
      walked = Jekyll::Rust.reader_walk(@site, "")
      pages  = Array(walked[:pages]).map(&:to_s)
      static = Array(walked[:static]).map(&:to_s)

      assert_includes pages, "about.html"
      assert_includes pages, "index.html"
      assert_includes static, "pgp.key"
      assert_includes static, "css/screen.css"
    end

    should "list directories with forward slashes" do
      walked = Jekyll::Rust.reader_walk(@site, "")
      dirs = Array(walked[:dirs]).map(&:to_s)

      assert_includes dirs, "assets"
      assert_includes dirs, "css"
      assert dirs.all? { |path| !path.include?("\\") },
             "expected all directory entries to use forward slashes"
    end
  end
end
