# frozen_string_literal: true

require_relative "rust"

module Jekyll
  module Utils
    extend self
    autoload :Ansi, "jekyll/utils/ansi"
    autoload :Exec, "jekyll/utils/exec"
    autoload :Internet, "jekyll/utils/internet"
    autoload :Platforms, "jekyll/utils/platforms"
    autoload :ThreadEvent, "jekyll/utils/thread_event"
    autoload :WinTZ, "jekyll/utils/win_tz"

    # Constants for use in #slugify
    SLUGIFY_MODES = %w(raw default pretty ascii latin).freeze
    SLUGIFY_RAW_REGEXP = Regexp.new('\\s+').freeze
    SLUGIFY_DEFAULT_REGEXP = Regexp.new("[^\\p{M}\\p{L}\\p{Nd}]+").freeze
    SLUGIFY_PRETTY_REGEXP = Regexp.new("[^\\p{M}\\p{L}\\p{Nd}._~!$&'()+,;=@]+").freeze
    SLUGIFY_ASCII_REGEXP = Regexp.new("[^[A-Za-z0-9]]+").freeze

    # Takes a slug and turns it into a simple title.
    def titleize_slug(slug)
      Jekyll::Rust.titleize_slug(slug)
    end

    # Non-destructive version of deep_merge_hashes! See that method.
    #
    # Returns the merged hashes.
    def mergable?(value)
      Jekyll::Rust.mergable?(value)
    end

    def duplicable?(obj)
      Jekyll::Rust.duplicable?(obj)
    end

    def deep_merge_hashes(master_hash, other_hash)
      Jekyll::Rust.deep_merge_hashes(master_hash, other_hash)
    end

    # Merges a master hash with another hash, recursively.
    # target - the "parent" hash whose values will be overridden
    # overwrite - the other hash whose values will be persisted after the merge
    def deep_merge_hashes!(target, overwrite)
      Jekyll::Rust.deep_merge_hashes_bang(target, overwrite)
    end


    # Read array from the supplied hash favouring the singular key
    # and then the plural key, and handling any nil entries.
    #
    # hash - the hash to read from
    # singular_key - the singular key
    # plural_key - the plural key
    #
    # Returns an array
    def pluralized_array_from_hash(hash, singular_key, plural_key)
      Jekyll::Rust.pluralized_array_from_hash(hash, singular_key, plural_key)
    end

    # Apply #to_sym to all keys in the hash
    #
    # hash - the hash to which to apply this transformation
    #
    # Returns a new hash with symbolized keys
    def symbolize_hash_keys(hash)
      Jekyll::Rust.symbolize_hash_keys(hash)
    end

    # Apply #to_s to all keys in the Hash
    #
    # hash - the hash to which to apply this transformation
    #
    # Returns a new hash with stringified keys
    def stringify_hash_keys(hash)
      Jekyll::Rust.stringify_hash_keys(hash)
    end

    # Parse a date/time and throw an error if invalid
    #
    # input - the date/time to parse
    # msg - (optional) the error message to show the user
    #
    # Returns the parsed date if successful, throws a FatalException
    # if not
    def parse_date(input, msg = "Input could not be parsed.")
      timezone = ENV["TZ"]
      key = [input, timezone]
      @parse_date_cache ||= {}
      @parse_date_cache.fetch(key) do
        @parse_date_cache[key] = Jekyll::Rust.parse_time(input, msg)
      end
    end

    # Determines whether a given file has
    #
    # Returns true if the YAML front matter is present.
    # rubocop: disable Naming/PredicateName
    def has_yaml_header?(file)
      Jekyll::Rust.has_yaml_header?(file)
    end

    # Determine whether the given content string contains Liquid Tags or Variables
    #
    # Returns true is the string contains sequences of `{%` or `{{`
    def has_liquid_construct?(content)
      Jekyll::Rust.has_liquid_construct?(content)
    end
    # rubocop: enable Naming/PredicateName

    # Slugify a filename or title.
    #
    # string - the filename or title to slugify
    # mode - how string is slugified
    # cased - whether to replace all uppercase letters with their
    # lowercase counterparts
    #
    # When mode is "none", return the given string.
    #
    # When mode is "raw", return the given string,
    # with every sequence of spaces characters replaced with a hyphen.
    #
    # When mode is "default" or nil, non-alphabetic characters are
    # replaced with a hyphen too.
    #
    # When mode is "pretty", some non-alphabetic characters (._~!$&'()+,;=@)
    # are not replaced with hyphen.
    #
    # When mode is "ascii", some everything else except ASCII characters
    # a-z (lowercase), A-Z (uppercase) and 0-9 (numbers) are not replaced with hyphen.
    #
    # When mode is "latin", the input string is first preprocessed so that
    # any letters with accents are replaced with the plain letter. Afterwards,
    # it follows the "default" mode of operation.
    #
    # If cased is true, all uppercase letters in the result string are
    # replaced with their lowercase counterparts.
    #
    # Examples:
    #   slugify("The _config.yml file")
    #   # => "the-config-yml-file"
    #
    #   slugify("The _config.yml file", "pretty")
    #   # => "the-_config.yml-file"
    #
    #   slugify("The _config.yml file", "pretty", true)
    #   # => "The-_config.yml file"
    #
    #   slugify("The _config.yml file", "ascii")
    #   # => "the-config-yml-file"
    #
    #   slugify("The _config.yml file", "latin")
    #   # => "the-config-yml-file"
    #
    # Returns the slugified string.
    def slugify(string, mode: nil, cased: false)
      return nil if string.nil?

      Jekyll::Rust.slugify(string, mode, cased)
    end

    # Add an appropriate suffix to template so that it matches the specified
    # permalink style.
    #
    # template - permalink template without trailing slash or file extension
    # permalink_style - permalink style, either built-in or custom
    #
    # The returned permalink template will use the same ending style as
    # specified in permalink_style.  For example, if permalink_style contains a
    # trailing slash (or is :pretty, which indirectly has a trailing slash),
    # then so will the returned template.  If permalink_style has a trailing
    # ":output_ext" (or is :none, :date, or :ordinal) then so will the returned
    # template.  Otherwise, template will be returned without modification.
    #
    # Examples:
    #   add_permalink_suffix("/:basename", :pretty)
    #   # => "/:basename/"
    #
    #   add_permalink_suffix("/:basename", :date)
    #   # => "/:basename:output_ext"
    #
    #   add_permalink_suffix("/:basename", "/:year/:month/:title/")
    #   # => "/:basename/"
    #
    #   add_permalink_suffix("/:basename", "/:year/:month/:title")
    #   # => "/:basename"
    #
    # Returns the updated permalink template
    def add_permalink_suffix(template, permalink_style)
      Jekyll::Rust.add_permalink_suffix(template, permalink_style)
    end

    def ensure_leading_slash(path)
      Jekyll::Rust.ensure_leading_slash(path)
    end

    # Work the same way as Dir.glob but separating the input into two parts
    # ('dir' + '/' + 'pattern') to make sure the first part('dir') does not act
    # as a pattern.
    #
    # For example, Dir.glob("path[/*") always returns an empty array,
    # because the method fails to find the closing pattern to '[' which is ']'
    #
    # Examples:
    #   safe_glob("path[", "*")
    #   # => ["path[/file1", "path[/file2"]
    #
    #   safe_glob("path", "*", File::FNM_DOTMATCH)
    #   # => ["path/.", "path/..", "path/file1"]
    #
    #   safe_glob("path", ["**", "*"])
    #   # => ["path[/file1", "path[/folder/file2"]
    #
    # dir      - the dir where glob will be executed under
    #           (the dir will be included to each result)
    # patterns - the patterns (or the pattern) which will be applied under the dir
    # flags    - the flags which will be applied to the pattern
    #
    # Returns matched paths
    def safe_glob(dir, patterns, flags = 0)
      return [] unless Dir.exist?(dir)

      Jekyll::Rust.safe_glob(dir, patterns, flags)
    end

    # Returns merged option hash for File.read of self.site (if exists)
    # and a given param
    def merged_file_read_opts(site, opts)
      Jekyll::Rust.merged_file_read_opts(site, opts)
    end

    private

    # Replace each character sequence with a hyphen.
    #
    # See Utils#slugify for a description of the character sequence specified
    # by each mode.
    def replace_character_sequence_with_hyphen(string, mode: "default")
      replaceable_char =
        case mode
        when "raw"
          SLUGIFY_RAW_REGEXP
        when "pretty"
          # "._~!$&'()+,;=@" is human readable (not URI-escaped) in URL
          # and is allowed in both extN and NTFS.
          SLUGIFY_PRETTY_REGEXP
        when "ascii"
          # For web servers not being able to handle Unicode, the safe
          # method is to ditch anything else but latin letters and numeric
          # digits.
          SLUGIFY_ASCII_REGEXP
        else
          SLUGIFY_DEFAULT_REGEXP
        end

      # Strip according to the mode
      string.gsub(replaceable_char, "-")
    end
  end
end
