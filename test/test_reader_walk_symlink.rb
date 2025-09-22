# frozen_string_literal: true

require "helper"

class TestReaderWalkSymlink < JekyllUnitTest
  context "reader_walk with symlinks in safe mode" do
    setup do
      @site = fixture_site
      @src = @site.source
      require "tmpdir"
      @outside = Dir.mktmpdir("jekyll_outside")
      File.write(File.join(@outside, "evil.md"), <<~MD)
        ---
        title: Evil
        ---
        outside
      MD
      FileUtils.mkdir_p(File.join(@src, "symlink-test"))
      @link = File.join(@src, "symlink-test", "evil.md")
      begin
        File.symlink(File.join(@outside, "evil.md"), @link)
      rescue NotImplementedError, Errno::EACCES
        omit("symlink not supported in this environment")
      end
    end

    teardown do
      FileUtils.rm_f(@link) if @link && File.exist?(@link)
      FileUtils.rm_rf(@outside) if @outside && Dir.exist?(@outside)
    end

    should "exclude symlinked file outside source when safe" do
      @site.safe = true
      walked = Jekyll::Rust.reader_walk(@site, "")
      pages  = Array(walked[:pages]).map(&:to_s)
      stat   = Array(walked[:static]).map(&:to_s)
      refute_includes pages, "symlink-test/evil.md"
      refute_includes stat,  "symlink-test/evil.md"
    end
  end
end
