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

      def document_populate_categories(data)
        ensure_loaded!
        Bridge.document_populate_categories(data)
      end

      def document_populate_tags(data)
        ensure_loaded!
        Bridge.document_populate_tags(data)
      end

      def document_title_parts(relative_path, basename_without_ext)
        ensure_loaded!
        Bridge.document_title_parts(relative_path, basename_without_ext)
      end
      def document_metadata(path, relative_path, special_dir)
        ensure_loaded!
        Bridge.document_metadata(path, relative_path, special_dir)
      end


      def document_read(path, file_opts)
        ensure_loaded!
        Bridge.document_read(path, file_opts)
      end

      def safe_glob(dir, patterns, flags)
        ensure_loaded!
        Bridge.safe_glob(dir, patterns, flags)
      end

      def parse_date(input, message = nil)
        ensure_loaded!
        Bridge.parse_time(input, message)
      end

      def parse_time(input, message = nil)
        ensure_loaded!
        Bridge.parse_time(input, message)
      end

      def static_file_basename(name, extname)
        ensure_loaded!
        Bridge.static_file_basename(name, extname)
      end

      def static_file_cleaned_relative_path(relative_path, extname, collection_dir)
        ensure_loaded!
        Bridge.static_file_cleaned_relative_path(relative_path, extname, collection_dir)
      end

      def static_file_write(src_path, dest_path, mtime, safe, production)
        ensure_loaded!
        Bridge.static_file_write(src_path, dest_path, mtime, safe, production)
      end

      def static_file_destination_rel_dir(url, dir, has_collection)
        ensure_loaded!
        Bridge.static_file_destination_rel_dir(url, dir, has_collection)
      end
      def static_file_write_batch(jobs, safe, production)
        ensure_loaded!
        Bridge.static_file_write_batch(jobs, safe, production)
      end


      def static_file_mtime_get(path)
        ensure_loaded!
        Bridge.static_file_mtime_get(path)
      end

      def static_file_mtime_set(path, mtime)
        ensure_loaded!
        Bridge.static_file_mtime_set(path, mtime)
      end

      def static_file_mtimes_reset
        ensure_loaded!
        Bridge.static_file_mtimes_reset
      end

      def static_file_mtimes_snapshot
        ensure_loaded!
        Bridge.static_file_mtimes_snapshot
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

      def ensure_leading_slash(path)
        ensure_loaded!
        Bridge.ensure_leading_slash(path)
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

      # Engine entrypoint
      def engine_build_site(site)
        ensure_loaded!
        Bridge.engine_build_site(site)
      end

      def engine_build_process(options)
        ensure_loaded!
        Bridge.engine_build_process(options)
      end

      def engine_clean_process(options)
        ensure_loaded!
        Bridge.engine_clean_process(options)
      end

      def engine_generate(site)
        ensure_loaded!
        Bridge.engine_generate(site)
      end


      # URL filters helpers
      def url_filters_absolute_url(site, input)
        ensure_loaded!
        Bridge.url_filters_absolute_url(site, input)
      end

      def url_filters_relative_url(site, input)
        ensure_loaded!
        Bridge.url_filters_relative_url(site, input)
      end

      def url_filters_strip_index(input)
        ensure_loaded!
        Bridge.url_filters_strip_index(input)
      end

      def url_filters_join_relative(baseurl, input)
        ensure_loaded!
        Bridge.url_filters_join_relative(baseurl, input)
      end


      def entry_filter(site, entries, base_directory)
        ensure_loaded!
        Bridge.entry_filter(site, entries, base_directory)
      end

      def reader_classify(site, base_directory)
        ensure_loaded!
        Bridge.reader_classify(site, base_directory)
      end

      def reader_walk(site, dir = "")
        ensure_loaded!
        Bridge.reader_walk(site, dir)
      end

      def reader_get_entries(site, dir, subfolder)
        ensure_loaded!
        Bridge.reader_get_entries(site, dir, subfolder)
      end

      def reader_get_entries_posts(site, dir, subfolder)
        ensure_loaded!
        Bridge.reader_get_entries_posts(site, dir, subfolder)
      end

      def reader_get_entries_drafts(site, dir, subfolder)
        ensure_loaded!
        Bridge.reader_get_entries_drafts(site, dir, subfolder)
      end

      def data_reader_entries(site, dir)
        ensure_loaded!
        Bridge.data_reader_entries(site, dir)
      end

      def layout_entries(site, dir)
        ensure_loaded!
        Bridge.layout_entries(site, dir)
      end

      def merged_file_read_opts(site, opts)
        ensure_loaded!
        Bridge.merged_file_read_opts(site, opts)
      end

      def has_yaml_header?(path)
        ensure_loaded!
        Bridge.has_yaml_header?(path)
      end

      # Front matter defaults helpers
      def frontmatter_applies_path(path, scope_path, site_source, collections_dir)
        ensure_loaded!
        Bridge.frontmatter_applies_path(path, scope_path, site_source, collections_dir)
      end

      def frontmatter_has_precedence(old_scope, new_scope)
        ensure_loaded!
        Bridge.frontmatter_has_precedence(old_scope, new_scope)
      end

      # Cleaner helpers
      def cleaner_existing_files(site_dest, keep_files)
        ensure_loaded!
        Bridge.cleaner_existing_files(site_dest, keep_files)
      end

      # Theme assets reader helpers
      def theme_assets_list(root)
        ensure_loaded!
        Bridge.theme_assets_list(root)
      end




      # Regenerator IO helpers
      def regenerator_read_metadata(metadata_file, disabled)
        ensure_loaded!
        Bridge.regenerator_read_metadata(metadata_file, disabled)
      end

      def regenerator_write_metadata(metadata_file, metadata, disabled)
        ensure_loaded!
        Bridge.regenerator_write_metadata(metadata_file, metadata, disabled)
      end


      def regenerator_existing_file_modified(this_obj, path)
        ensure_loaded!
        Bridge.regenerator_existing_file_modified(this_obj, path)
      end


      def regenerator_source_modified_or_dest_missing(this_obj, source_path, dest_path)
        ensure_loaded!
        Bridge.regenerator_source_modified_or_dest_missing(this_obj, source_path, dest_path)
      end

      def regenerator_modified(this_obj, path)
        ensure_loaded!
        Bridge.regenerator_modified(this_obj, path)
      end



      def normalize_whitespace(input)
        ensure_loaded!
        Bridge.normalize_whitespace(input)
      end
      def number_of_words(input, mode = nil)
        ensure_loaded!
        Bridge.number_of_words(input, mode)
      end

      def where_filter_fast(input, property, target)
        ensure_loaded!
        Bridge.where_filter_fast(input, property, target)
      end

      def where_exp_fast(input, variable, expression)
        ensure_loaded!
        Bridge.where_exp_fast(input, variable, expression)
      end

      def sort_filter_fast(input, property, nils)
        ensure_loaded!
        Bridge.sort_filter_fast(input, property, nils)
      end

      def group_by_fast(input, property)
        ensure_loaded!
        Bridge.group_by_fast(input, property)
      end

      def find_filter_fast(input, property, value)
        ensure_loaded!
        Bridge.find_filter_fast(input, property, value)
      end

      private

      def ensure_loaded!
        return if @loaded

        last_error = nil
        begin
          require "jekyll_core"
          @loaded = true
          return
        rescue LoadError => gem_error
          last_error = gem_error
        end

        path = ENV["JEKYLL_RUST_LIB"]
        if path && !path.empty?
          begin
            require File.expand_path(path)
            @loaded = true
            return
          rescue LoadError => e
            raise Errors::FatalException,
                  "Failed to load Rust extension from #{path}: #{e.message}"
          end
        end

        message = "Failed to load the Rust bridge via `require 'jekyll_core'`"
        message = "#{message}: #{last_error.message}" if last_error
        message = "#{message}.\nRun `script/rust-build` or set `JEKYLL_RUST_LIB` to the built cdylib path." \
          unless path && !path.empty?
        raise Errors::FatalException, message
      end
    end
  end
end
