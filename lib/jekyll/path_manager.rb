# frozen_string_literal: true

require_relative "rust"

module Jekyll
  # A singleton class that caches frozen instances of path strings returned from its methods.
  #
  # NOTE:
  #   This class exists because `File.join` allocates an Array and returns a new String on every
  #   call using **the same arguments**. Caching the result means reduced memory usage.
  #   However, the caches are never flushed so that they can be used even when a site is
  #   regenerating. The results are frozen to deter mutation of the cached string.
  #
  #   Therefore, employ this class only for situations where caching the result is necessary
  #   for performance reasons.
  #
  class PathManager
    # This class cannot be initialized from outside
    private_class_method :new

    class << self
      # Wraps `File.join` to cache the frozen result.
      # The heavy lifting is handled by the Rust implementation to minimise
      # repeated allocations.
      def join(base, item)
        Jekyll::Rust.path_manager_join(base, item)
      end

      # Ensures the questionable path is prefixed with the base directory and
      # returns a frozen string. Delegates to the Rust implementation for path
      # normalisation and caching semantics.
      def sanitized_path(base_directory, questionable_path)
        Jekyll::Rust.path_manager_sanitized_path(base_directory, questionable_path)
      end
    end
  end
end
