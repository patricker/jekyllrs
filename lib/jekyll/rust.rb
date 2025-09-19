# frozen_string_literal: true

require_relative "errors"

module Jekyll
  module Rust
    class << self
      def slugify(string, mode, cased)
        ensure_loaded!
        Bridge.slugify(string, mode, cased)
      end

      def path_manager_join(base, item)
        ensure_loaded!
        Bridge.path_manager_join(base, item)
      end

      def path_manager_sanitized_path(base_directory, questionable_path)
        ensure_loaded!
        Bridge.path_manager_sanitized_path(base_directory, questionable_path)
      end

      def document_basename(path)
        ensure_loaded!
        Bridge.document_basename(path)
      end

      def document_basename_without_ext(path)
        ensure_loaded!
        Bridge.document_basename_without_ext(path)
      end

      def document_cleaned_relative_path(relative_path, extname, relative_directory)
        ensure_loaded!
        Bridge.document_cleaned_relative_path(relative_path, extname, relative_directory)
      end

      def document_categories_from_path(relative_path, special_dir, basename)
        ensure_loaded!
        Bridge.document_categories_from_path(relative_path, special_dir, basename)
      end

      def safe_glob(dir, patterns, flags)
        ensure_loaded!
        Bridge.safe_glob(dir, patterns, flags)
      end

      def parse_date(input, message = nil)
        ensure_loaded!
        Bridge.parse_date(input, message)
      end

      def deep_merge_hashes(master_hash, other_hash)
        ensure_loaded!
        Bridge.deep_merge_hashes(master_hash, other_hash)
      end

      def deep_merge_hashes_bang(target, other_hash)
        ensure_loaded!
        Bridge.deep_merge_hashes_bang(target, other_hash)
      end

      def pluralized_array_from_hash(hash, singular_key, plural_key)
        ensure_loaded!
        Bridge.pluralized_array_from_hash(hash, singular_key, plural_key)
      end

      def symbolize_hash_keys(hash)
        ensure_loaded!
        Bridge.symbolize_hash_keys(hash)
      end

      def has_liquid_construct?(content)
        ensure_loaded!
        Bridge.has_liquid_construct?(content)
      end

      def stringify_hash_keys(hash)
        ensure_loaded!
        Bridge.stringify_hash_keys(hash)
      end

      def mergable?(value)
        ensure_loaded!
        Bridge.mergable?(value)
      end

      def duplicable?(value)
        ensure_loaded!
        Bridge.duplicable?(value)
      end

      def titleize_slug(slug)
        ensure_loaded!
        Bridge.titleize_slug(slug)
      end

      def add_permalink_suffix(template, permalink_style)
        ensure_loaded!
        Bridge.add_permalink_suffix(template, permalink_style)
      end

      def escape_path(path)
        ensure_loaded!
        Bridge.url_escape_path(path.to_s)
      end

      def unescape_path(path)
        ensure_loaded!
        Bridge.url_unescape_path(path.to_s)
      end

      def sanitize_url(path)
        ensure_loaded!
        Bridge.url_sanitize(path.to_s)
      end

      def url_generate_from_hash(template, placeholders)
        ensure_loaded!
        Bridge.url_generate_from_hash(template, placeholders)
      end

      def url_generate_from_drop(template, drop)
        ensure_loaded!
        Bridge.url_generate_from_drop(template, drop)
      end

      def entry_filter(site, entries, base_directory)
        ensure_loaded!
        Bridge.entry_filter(site, entries, base_directory)
      end

      def merged_file_read_opts(site, opts)
        ensure_loaded!
        Bridge.merged_file_read_opts(site, opts)
      end

      def has_yaml_header?(path)
        ensure_loaded!
        Bridge.has_yaml_header?(path)
      end

      private

      def ensure_loaded!
        return if @loaded

        path = ENV["JEKYLL_RUST_LIB"]
        if path.nil? || path.empty?
          raise Errors::FatalException,
                "JEKYLL_RUST_LIB is not set; build jekyll-core and export the cdylib path"
        end

        require File.expand_path(path)
        @loaded = true
      rescue LoadError => e
        raise Errors::FatalException,
              "Failed to load Rust extension from #{path}: #{e.message}"
      end
    end
  end
end
