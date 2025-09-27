# frozen_string_literal: true

require_relative "rust"

module Jekyll
  class Renderer
    attr_reader :document, :site
    attr_writer :layouts, :payload

    def initialize(site, document, site_payload = nil)
      @site = site
      @document = document
      @payload = site_payload
      @layouts = nil
      @converters = nil
      @output_ext = nil
    end

    def payload
      @payload ||= site.site_payload
    end

    def layouts
      @layouts || site.layouts
    end

    def converters
      @converters ||= Array(Jekyll::Rust.renderer_converters(site, document))
    end

    def run
      Jekyll::Rust.renderer_run(site, document, payload, layouts)
    end

    def convert(content)
      Jekyll::Rust.renderer_convert(site, document, content)
    end

    def render_liquid(content, payload, info, path = nil)
      Jekyll::Rust.renderer_render_liquid(site, document, content, payload, info, path)
    end

    def place_in_layouts(content, payload, info)
      Jekyll::Rust.renderer_place_in_layouts(site, document, content, payload, info, layouts)
    end

    def output_ext
      @output_ext ||= Jekyll::Rust.renderer_output_ext(site, document)
    end
  end
end
