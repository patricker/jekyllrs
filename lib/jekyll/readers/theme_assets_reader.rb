# frozen_string_literal: true

module Jekyll
  class ThemeAssetsReader
    attr_reader :site

    def initialize(site)
      @site = site
    end

    def read
      return unless site.theme&.assets_path

      Jekyll::Rust.theme_assets_list(site.theme.assets_path).each do |path|
        if File.symlink?(path)
          Jekyll.logger.warn "Theme reader:", "Ignored symlinked asset: #{path}"
        else
          read_theme_asset(path)
        end
      end
    end

    private

    def read_theme_asset(path)
      base = site.theme.root
      dir = File.dirname(path.sub("#{site.theme.root}/", ""))
      name = File.basename(path)

      if Utils.has_yaml_header?(path)
        append_unless_exists site.pages,
                             Jekyll::Page.new(site, base, dir, name)
      else
        append_unless_exists site.static_files,
                             Jekyll::StaticFile.new(site, base, "/#{dir}", name)
      end
    end

    def append_unless_exists(haystack, new_item)
      new_rel = new_item.relative_path.delete_prefix("/")
      new_url = new_item.respond_to?(:url) ? new_item.url.to_s : nil
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
