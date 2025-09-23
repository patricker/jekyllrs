# frozen_string_literal: true

module Jekyll
  class LayoutReader
    attr_reader :site

    def initialize(site)
      @site = site
      @layouts = {}
    end

    def read
      layout_entries.each do |layout_file|
        @layouts[layout_name(layout_file)] = \
          Layout.new(site, layout_directory, layout_file)
      end

      theme_layout_entries.each do |layout_file|
        @layouts[layout_name(layout_file)] ||= \
          Layout.new(site, theme_layout_directory, layout_file)
      end

      @layouts
    end

    def layout_directory
      @layout_directory ||= site.in_source_dir(site.config["layouts_dir"])
    end

    def theme_layout_directory
      @theme_layout_directory ||= site.theme.layouts_path if site.theme
    end

    private

    def layout_entries
      Array(Jekyll::Rust.layout_entries(site, layout_directory))
    end

    def theme_layout_entries
      theme_layout_directory ? Array(Jekyll::Rust.layout_entries(site, theme_layout_directory)) : []
    end

    def layout_name(file)
      file.split(".")[0..-2].join(".")
    end
  end
end
