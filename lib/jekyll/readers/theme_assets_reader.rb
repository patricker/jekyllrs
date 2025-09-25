# frozen_string_literal: true

module Jekyll
  class ThemeAssetsReader
    attr_reader :site

    def initialize(site)
      @site = site
    end

    def read
      return unless site.theme&.assets_path

      Jekyll::Rust.theme_assets_list(site.theme.assets_path).each do |entry|
        path, relative_path, symlink = entry
        if symlink
          Jekyll.logger.warn "Theme reader:", "Ignored symlinked asset: #{path}"
        else
          read_theme_asset(path, relative_path)
        end
      end
    end

    private

    def read_theme_asset(path, relative_path)
      base = site.theme.root
      dir = File.dirname(relative_path)
      name = File.basename(relative_path)
      dir = "" if dir == "."
      static_dir = dir.empty? ? "/" : "/#{dir}"

      if Utils.has_yaml_header?(path)
        page = Jekyll::Page.new(site, base, dir, name)
        append_unless_exists site.pages, page, relative_path, page.url.to_s
      else
        static_file = Jekyll::StaticFile.new(site, base, static_dir, name)
        append_unless_exists site.static_files, static_file, relative_path
      end
    end

    def append_unless_exists(haystack, new_item, relative_path, new_url = nil)
      new_rel = relative_path.delete_prefix("/")
      new_url ||= new_item.respond_to?(:url) ? new_item.url.to_s : nil
      # Prefer site content over theme content: if a site file exists on disk
      # at the same relative path, do not add the theme item regardless of
      # whether the site item has been materialized yet.
      site_path = File.join(site.source, new_rel)
      if File.exist?(site_path) || haystack.any? do |file|
           (file.relative_path.delete_prefix("/") == new_rel) ||
           (new_url && file.respond_to?(:url) && file.url.to_s == new_url)
         end
        Jekyll.logger.debug "Theme:",
                            "Ignoring #{new_item.relative_path} in theme due to existing file " \
                            "with that path in site."
        return
      end

      haystack << new_item
    end
  end
end
