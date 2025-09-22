# frozen_string_literal: true

require "helper"

class TestReaderClassify < JekyllUnitTest
  context "Reader classification and exclude behavior" do
    setup do
      @site = fixture_site
      @tmp_lib = File.join(@site.source, "tmp_lib")
      FileUtils.mkdir_p(@tmp_lib)
      File.write(File.join(@tmp_lib, "release-template.erb"), "date: <%= Time.now %>\n")
    end

    teardown do
      FileUtils.rm_rf(@tmp_lib)
    end

    should "exclude directories listed in config during classification" do
      @site.exclude = ["tmp_lib"]
      classified = Jekyll::Rust.reader_classify(@site, @tmp_lib)
      assert_equal [], Array(classified[:dirs])
      assert_equal [], Array(classified[:pages])
      assert_equal [], Array(classified[:static])
    end

    should "filter entries with exclude relative to site source when base is provided" do
      @site.exclude = ["tmp_lib"]
      entries = ["release-template.erb"]
      filtered = Jekyll::EntryFilter.new(@site, @tmp_lib).filter(entries)
      assert_equal [], filtered
    end
  end
end
