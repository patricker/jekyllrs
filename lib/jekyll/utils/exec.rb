# frozen_string_literal: true

require "open3"

module Jekyll
  module Utils
    module Exec
      extend self

      # Runs a program in a sub-shell.
      #
      # *args - a list of strings containing the program name and arguments
      #
      # Returns a Process::Status and a String of output in an array in
      # that order.
      def run(*args)
        # Capture stdout and stderr together to avoid deadlocks when one pipe fills.
        out, status = Open3.capture2e(*args)
        [status, out.strip]
      end
    end
  end
end
